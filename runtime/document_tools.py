"""Offline document processing; PDFium is isolated in this worker process."""
import hashlib,json,re,tempfile,zipfile,sqlite3
from collections.abc import Mapping
from pathlib import Path
from xml.etree import ElementTree as ET

W='{http://schemas.openxmlformats.org/wordprocessingml/2006/main}'
XML='{http://www.w3.org/XML/1998/namespace}'
MAX_XML=32*1024*1024
def docx_parts(path):
    archive=zipfile.ZipFile(path)
    entries=archive.infolist()
    if len(entries)>10000 or sum(i.file_size for i in entries)>128*1024*1024:
        archive.close();raise ValueError('DOCX слишком большой после распаковки (лимит 128 МБ / 10000 элементов).')
    if len({i.filename for i in entries})!=len(entries):archive.close();raise ValueError('DOCX содержит повторяющиеся элементы.')
    names=[n for n in archive.namelist() if n=='word/document.xml' or re.fullmatch(r'word/(header\d+|footer\d+|footnotes|endnotes)\.xml',n)]
    if 'word/document.xml' not in names:archive.close();raise ValueError('В DOCX нет word/document.xml.')
    trees={}
    try:
        for name in sorted(names,key=lambda n:(n!='word/document.xml',n)):
            if archive.getinfo(name).file_size>MAX_XML:raise ValueError('XML внутри DOCX больше 32 МБ.')
            data=archive.read(name)
            if b'<!DOCTYPE' in data.upper() or b'<!ENTITY' in data.upper():raise ValueError('DTD и сущности XML не поддерживаются.')
            trees[name]=ET.fromstring(data)
        return archive,trees
    except Exception:archive.close();raise

def paragraph_text(p):
    def walk(node):
        if node is not p and node.tag==W+'p':return
        if node.tag==W+'t':yield node.text or ''
        elif node.tag==W+'tab':yield '\t'
        elif node.tag in (W+'br',W+'cr'):yield '\n'
        for child in node:yield from walk(child)
    return ''.join(walk(p))

def iter_paragraph_units(tree,name):
    """Merge adjacent text with identical formatting, retaining every XML node."""
    parents={child:parent for parent in tree.iter() for child in parent}
    ids={node:name+'#'+str(i) for i,node in enumerate(tree.iter(W+'t'))}
    for index,p in enumerate(tree.iter(W+'p')):
        segments=[]
        def walk(node):
            if node is not p and node.tag==W+'p':return
            if node.tag in (W+'pPr',W+'rPr'):return
            if node.tag in (W+'drawing',W+'pict',W+'object'):
                segments.append({'kind':'fixed','text':''});return
            if node.tag==W+'t':
                parent=parents.get(node);run=parent
                while run is not None and run.tag!=W+'r':run=parents.get(run)
                props=run.find(W+'rPr') if run is not None else None
                key=(id(parent),) if run is None else (id(parents.get(run)),tuple(sorted((k,v) for k,v in run.attrib.items() if not k.startswith(W+'rsid'))),ET.tostring(props) if props is not None else b'')
                text=node.text or ''
                if segments and segments[-1].get('_key')==key:
                    segments[-1]['text']+=text;segments[-1]['run_ids'].append(ids[node])
                else:segments.append({'kind':'text','text':text,'run_ids':[ids[node]],'_key':key})
                return
            if node.tag in (W+'tab',W+'br',W+'cr') or (len(node)==0 and node is not p and node.tag!=W+'r'):
                segments.append({'kind':'fixed','text':'\t' if node.tag==W+'tab' else '\n' if node.tag in (W+'br',W+'cr') else ''})
                return
            for child in node:walk(child)
        walk(p)
        text=''.join(part['text'] for part in segments)
        if not text.strip():continue
        for number,part in enumerate(segments):part.pop('_key',None);part['id']='lg'+str(number)
        yield {'id':name+'#p'+str(index),'text':text,'marked':len(segments)!=1,'segments':segments}

def paragraph_units(tree,name):return list(iter_paragraph_units(tree,name))


def paragraph_input(unit):
    if not unit['marked']:return unit['text']
    from xml.sax.saxutils import escape
    return '<p>'+''.join('<span id="'+s['id']+'">'+escape(s['text'])+'</span>' if s['kind']=='text' else '<x id="'+s['id']+'" />' for s in unit['segments'])+'</p>'


def paragraph_translation(unit,translated):
    if not isinstance(translated,str) or not translated.strip():raise ValueError('Пустой перевод абзаца.')
    text_segments=[s for s in unit['segments'] if s['kind']=='text']
    if not unit['marked']:return {text_segments[0]['id']:translated}
    value=translated.strip()
    # Some models wrap otherwise valid HTML in one Markdown code fence.
    match=re.fullmatch(r'```(?:html|xml)?\s*\n(.*?)\n```',value,re.S|re.I)
    if match:value=match[1].strip()
    if '<!' in value or '<?' in value:raise ValueError('Модель добавила недопустимую разметку абзаца.')
    try:root=ET.fromstring(value)
    except ET.ParseError as error:raise ValueError('Модель повредила разметку абзаца.') from error
    if root.tag!='p' or root.attrib or (root.text or '').strip():raise ValueError('Модель изменила контейнер абзаца.')
    if len(root)!=len(unit['segments']):raise ValueError('Модель потеряла или добавила метки форматирования.')
    def inline_space(value):return value if '\n' not in value and '\r' not in value else ''
    mapping={};pending=inline_space(root.text or '')
    for child,segment in zip(root,unit['segments']):
        tag='span' if segment['kind']=='text' else 'x'
        if child.tag!=tag or child.attrib!={'id':segment['id']} or len(child) or (child.tail or '').strip():raise ValueError('Модель изменила порядок или состав меток форматирования.')
        value=child.text or ''
        if segment['kind']=='fixed':
            if value:raise ValueError('Модель изменила служебную метку абзаца.')
            pending+=inline_space(child.tail or '')
        else:
            mapping[segment['id']]=pending+value+inline_space(child.tail or '');pending=''
    if pending and mapping:mapping[next(reversed(mapping))]+=pending
    if not ''.join(mapping.values()).strip():raise ValueError('Модель удалила весь текст абзаца.')
    return mapping


class CheckpointTranslations(Mapping):
    def __init__(self,database,job):
        self.db=sqlite3.connect(Path(database).resolve().as_uri()+'?mode=ro',uri=True)
        self.db.execute('PRAGMA query_only=ON');self.job=job
        if self.db.execute('PRAGMA user_version').fetchone()[0] not in (10,11):self.db.close();raise ValueError('Неизвестная версия сохранённых переводов.')
    def __len__(self):return self.db.execute('SELECT COUNT(*) FROM document_parts WHERE job=?',(self.job,)).fetchone()[0]
    def __iter__(self):return (row[0] for row in self.db.execute('SELECT unit FROM document_parts WHERE job=? ORDER BY unit',(self.job,)))
    def __getitem__(self,key):
        row=self.db.execute('SELECT result FROM document_parts WHERE job=? AND unit=?',(self.job,key)).fetchone()
        if row is None:raise KeyError(key)
        return json.loads(row[0])

def docx_spool(path,output):
    archive,trees=docx_parts(path)
    try:
        count=0;sample='';digest=hashlib.sha256()
        with Path(output).open('wb') as stream:
            for name,tree in trees.items():
                for unit in iter_paragraph_units(tree,name):
                    unit['input']=paragraph_input(unit)
                    block=(json.dumps(unit,ensure_ascii=False,separators=(',',':'),sort_keys=True)+'\n').encode('utf-8')
                    stream.write(block);digest.update(block);count+=1
                    if len(sample)<4096:sample+=(unit['text']+'\n')[:4096-len(sample)]
        return {'format':'docx','translation_mode':'paragraph-v2','units_count':count,'paragraphs_count':count,'complete':True,'plan_sha256':digest.hexdigest(),'sample_text':sample,'plan_storage':'jsonl-v1','warnings':['Перевод абзацами с проверкой меток форматирования. Разрывы страниц могут сместиться; текст в изображениях и диаграммах не переводится.']}
    finally:archive.close()

def docx_read(path,plan=False,paragraph_mode=False,offset=0,max_characters=20000):
    if plan:raise ValueError('Для плана перевода используйте дисковую подготовку prepare.')
    if offset<0 or not 1<=max_characters<=20000:raise ValueError('Неверный диапазон текста DOCX.')
    archive,trees=docx_parts(path)
    try:
        units_count=0;paragraphs_count=0;paragraphs=[]
        for name,tree in trees.items():

            if name!='word/document.xml':
                label='Верхний колонтитул' if '/header' in name else 'Нижний колонтитул' if '/footer' in name else 'Концевые сноски' if '/endnotes' in name else 'Сноски'
                paragraphs.append('\n['+label+']')
            for index,p in enumerate(tree.iter(W+'p')):
                text=paragraph_text(p)
                if not text.strip():continue
                paragraphs_count+=1
                style=p.find('./'+W+'pPr/'+W+'pStyle');style=style.get(W+'val','') if style is not None else ''
                level=re.search(r'(?:heading|заголовок)\s*([1-6])',style,re.I)
                paragraphs.append(('#'*int(level[1])+' ' if level else '')+text)
            for index,t in enumerate(tree.iter(W+'t')):
                if (t.text or '').strip():units_count+=1
        text='\n\n'.join(paragraphs)
        result={'format':'docx','units_count':units_count,'complete':True,'warnings':['Перевод выполняется по текстовым фрагментам с сохранением таблиц и стилей. Контекст между фрагментами и исходные разрывы страниц могут измениться. Текст внутри изображений и диаграмм не переводится.']}
        result['paragraphs_count']=paragraphs_count
        if paragraph_mode:
            result['translation_mode']='paragraph-v2'
            result['warnings']=['Перевод целыми абзацами с сохранением таблиц, стилей и ссылок. Метки форматирования проверяются перед сохранением; при ошибке документ не создаётся. Разрывы страниц могут сместиться; текст внутри изображений и диаграмм не переводится.']
        if offset>len(text):raise ValueError('Начало диапазона вне текста DOCX.')
        end=min(len(text),offset+max_characters)
        result.update({'text':text[offset:end],'start_offset':offset,'end_offset':end,'total_characters':len(text),'next_offset':end if end<len(text) else None,'has_more':end<len(text),'complete':offset==0 and end==len(text),'range_complete':end==len(text)})
        return result
    finally:archive.close()

def docx_export(path,output,translations,paragraph_mode=False):
    archive,trees=docx_parts(path)
    try:
        if paragraph_mode:
            expected_units={u['id'] for name,tree in trees.items() for u in iter_paragraph_units(tree,name)}
            if not isinstance(translations,Mapping) or set(translations)!=expected_units:raise ValueError('Набор переводов не совпадает с исходными абзацами DOCX.')
            units=(u for name,tree in trees.items() for u in iter_paragraph_units(tree,name))
            nodes={name+'#'+str(i):node for name,tree in trees.items() for i,node in enumerate(tree.iter(W+'t'))}
            expected=set()
            for unit in units:
                record=translations[unit['id']]
                if not isinstance(record,dict) or set(record)!={'text','unchanged'} or not isinstance(record['unchanged'],bool):raise ValueError('Неверная запись перевода абзаца.')
                if record['unchanged']:
                    if record['text']!=paragraph_input(unit):raise ValueError('Исходный абзац изменён без перевода.')
                    continue
                values=paragraph_translation(unit,record['text'])
                for segment in unit['segments']:
                    if segment['kind']!='text':continue
                    for i,key in enumerate(segment['run_ids']):
                        node=nodes[key];node.text=values[segment['id']] if i==0 else '';node.set(XML+'space','preserve');expected.add(key)
        else:
            expected={name+'#'+str(i) for name,tree in trees.items() for i,t in enumerate(tree.iter(W+'t')) if (t.text or '').strip()}
            if set(translations)!=expected:raise ValueError('Набор переводов не совпадает с исходными фрагментами DOCX.')
            for name,tree in trees.items():
                for index,t in enumerate(tree.iter(W+'t')):
                    key=name+'#'+str(index)
                    if key in translations:
                        if not isinstance(translations[key],str):raise ValueError('Перевод должен быть строкой.')
                        t.text=translations[key];t.set(XML+'space','preserve')
        # Preserve declarations used only in attribute values such as mc:Ignorable.
        import io
        declarations={}
        for name in trees:
            declarations[name]={}
            for _,pair in ET.iterparse(io.BytesIO(archive.read(name)),events=['start-ns']):
                prefix,uri=pair
                declarations[name][prefix]=uri
                try:ET.register_namespace(prefix,uri)
                except ValueError:pass
        with zipfile.ZipFile(output,'w',compression=zipfile.ZIP_DEFLATED) as target:
            for info in archive.infolist():
                if info.filename in trees:
                    data=ET.tostring(trees[info.filename],encoding='utf-8',xml_declaration=True)
                    begin=data.index(b'\n')+1;end=data.index(b'>',begin)
                    from xml.sax.saxutils import quoteattr
                    missing=[]
                    for prefix,uri in declarations[info.filename].items():
                        attr='xmlns'+(':'+prefix if prefix else '')
                        if prefix!='xml' and (attr+'=').encode() not in data[begin:end]:missing.append(' '+attr+'='+quoteattr(uri))
                    insert=end-1 if data[end-1:end]==b'/' else end
                    data=data[:insert]+''.join(missing).encode('utf-8')+data[insert:]
                    ET.fromstring(data)
                else:data=archive.read(info.filename)
                target.writestr(info,data)
        with zipfile.ZipFile(output) as check:
            if check.testzip() is not None:raise ValueError('Проверка результата DOCX не прошла.')
        return {'verified':True,'translated_units':len(expected)}
    finally:archive.close()

def pdf_read(path,start=1,count=5,ocr=False,language='en-US',plan=False,offset=0,max_characters=20000):
    try:import pypdfium2 as pdfium
    except ImportError:raise ValueError('Модуль PDF не установлен. Обновите компоненты приложения (pypdfium2).')
    if not 1<=count<=5 or start<1:raise ValueError('PDF: начальная страница от 1, за один раз от 1 до 5 страниц.')
    if offset<0 or not 1<=max_characters<=20000:raise ValueError('Неверный диапазон текста PDF.')
    document=pdfium.PdfDocument(path if isinstance(path,bytes) else str(path))
    try:
        total=len(document)
        if start>total:raise ValueError(f'В PDF {total} страниц; начальная страница вне диапазона.')
        units=[];warnings=[];ocr_pages=[]
        for number in range(start,min(total,start+count-1)+1):
            page=document[number-1]
            try:
                textpage=page.get_textpage()
                try:text=textpage.get_text_range().replace('\r\n','\n').strip()
                finally:textpage.close()
                if not text and ocr:
                    import media_tools
                    width,height=page.get_size();scale=min(2.0,2800/max(width,height))
                    bitmap=page.render(scale=scale)
                    try:
                        with tempfile.TemporaryDirectory(prefix='local-pdf-ocr-') as temporary:
                            image=Path(temporary)/'page.png';bitmap.to_pil().save(image)
                            text=media_tools.image_text(image,language)
                        ocr_pages.append(number)
                    finally:bitmap.close()
                if not text:warnings.append(f'Страница {number}: текст не извлечён. Если это скан, включите OCR; смешанные страницы могут требовать ручного OCR области.')
                units.append({'id':str(number),'page':number,'text':text})
            finally:page.close()

        end=units[-1]['page'];result={'format':'pdf','total_pages':total,'start_page':start,'end_page':end,'has_more':end<total,'next_page':end+1 if end<total else None,'complete':start==1 and end==total,'warnings':warnings+['PDF переводится в Markdown по страницам; исходная вёрстка и таблицы PDF не восстанавливаются.'],'ocr_pages':ocr_pages}
        if plan:result['units']=units
        else:
            text='\n\n'.join(f"## Страница {u['page']}\n\n{u['text']}" for u in units)
            if offset>len(text):raise ValueError('Начало диапазона вне текста PDF.')
            text_end=min(len(text),offset+max_characters);more_text=text_end<len(text)
            result.update({'text':text[offset:text_end],'text_scope':{'start_page':start,'max_pages':count},'start_offset':offset,'end_offset':text_end,'total_characters':len(text),'next_offset':text_end if more_text else None,'has_more_text':more_text,'has_more':more_text or end<total,'complete':result['complete'] and offset==0 and not more_text,'range_complete':not more_text})
        return result
    finally:document.close()

class SourceChanged(ValueError):code='source_changed'

def process(request):
    operation=request.get('operation','extract')
    if operation=='validate_paragraph':
        try:paragraph_translation(request['unit'],request['translation']);return {'valid':True}
        except ValueError as error:return {'valid':False,'validation_error':str(error)}
    path=Path(request['path'])
    if not path.is_file() or path.stat().st_size>80*1024*1024:raise ValueError('Документ отсутствует или больше 80 МБ.')
    if operation=='export':
        if path.suffix.lower()!='.docx':raise ValueError('Экспорт структуры поддерживается только для DOCX.')
        source_bytes=path.read_bytes()
        if hashlib.sha256(source_bytes).hexdigest()!=request.get('source_sha256'):raise ValueError('Исходный DOCX изменён во время перевода. Повторите операцию.')
        import io
        mode=request.get('translation_mode')=='paragraph-v2'
        translations=CheckpointTranslations(request['checkpoint_database'],request['checkpoint']) if request.get('checkpoint_database') else request['translations']
        try:
            result=docx_export(io.BytesIO(source_bytes),Path(request['output']),translations,mode)
            if mode and all(v['unchanged'] for v in translations.values()):Path(request['output']).write_bytes(source_bytes)
            return result
        finally:
            if isinstance(translations,CheckpointTranslations):translations.db.close()
    plan=operation=='plan'
    if path.suffix.lower()=='.docx':
        import io
        source_bytes=path.read_bytes()
        if len(source_bytes)>80*1024*1024:raise ValueError('Документ больше 80 МБ.')
        result=docx_spool(io.BytesIO(source_bytes),request['_plan_path']) if operation=='prepare' else docx_read(io.BytesIO(source_bytes),plan,bool(request.get('paragraph_mode')),int(request.get('text_offset',0)),int(request.get('max_characters',20000)))
    elif path.suffix.lower()=='.pdf':
        source_bytes=path.read_bytes()
        result=pdf_read(source_bytes,int(request.get('start_page',1)),int(request.get('max_pages',5)),bool(request.get('ocr',False)),request.get('language') or 'en-US',plan or operation=='prepare',int(request.get('text_offset',0)),int(request.get('max_characters',20000)))
        if operation=='prepare':
            units=result.pop('units');digest=hashlib.sha256();sample=''
            with Path(request['_plan_path']).open('wb') as stream:
                for unit in units:
                    block=(json.dumps(unit,ensure_ascii=False,separators=(',',':'),sort_keys=True)+'\n').encode('utf-8');stream.write(block);digest.update(block)
                    if len(sample)<4096:sample+=(unit['text']+'\n')[:4096-len(sample)]
            result.update({'units_count':len(units),'plan_sha256':digest.hexdigest(),'sample_text':sample,'plan_storage':'jsonl-v1'})
    else:raise ValueError('Поддерживаются PDF и DOCX.')
    result['source_sha256']=hashlib.sha256(source_bytes).hexdigest()
    if request.get('source_sha256') and request['source_sha256']!=result['source_sha256']:raise SourceChanged('Документ изменился между диапазонами. Откройте его заново; прежний текст сохранён в истории.')
    return result
