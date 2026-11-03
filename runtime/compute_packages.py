"""Fetch matching official Ollama GPU libraries, without bundling weights.

HTTP byte ranges avoid downloading the entire multi-gigabyte release archive.
Every ZIP member is CRC checked by zipfile; extracted paths are allowlisted.
"""
import argparse,hashlib,io,json,os,re,shutil,sys,time,urllib.request,uuid,zipfile
from pathlib import Path,PurePosixPath

def strong_etag(value):
    return value is not None and re.fullmatch(r'"[\x21\x23-\x7e\x80-\xff]*"',value) is not None

class RangeZip(io.RawIOBase):
    block_size=4*1024*1024
    def __init__(self,url,cache_directory=None):
        super().__init__();self.opener=urllib.request.build_opener(urllib.request.ProxyHandler({}));self.url=url;self.pos=0;self.cache_start=-1;self.cache=b'';self.etag=None;self.modified=None;self.disk=None
        original_url=url
        with self._open(0,0) as response:
            match=re.fullmatch(r'bytes 0-0/(\d+)',response.headers.get('Content-Range',''))
            if response.status!=206 or not match:raise RuntimeError('Server must support HTTP byte ranges')
            self.size=int(match[1]);self.url=response.geturl();self.etag=response.headers.get('ETag');self.modified=response.headers.get('Last-Modified')
            if len(response.read(2))!=1:raise RuntimeError('Invalid archive length response')
        # A Last-Modified timestamp/weak ETag cannot identify bytes across restarts.
        # Keep those sessions usable, but do not reuse their blocks from disk.
        if cache_directory is not None and strong_etag(self.etag):
            base=Path(cache_directory).absolute()
            if base.resolve()!=base:raise RuntimeError('Download cache follows a link')
            base.mkdir(parents=True,exist_ok=True)
            identity={'version':1,'url':original_url,'etag':self.etag,'size':self.size,'block_size':self.block_size}
            key=hashlib.sha256(json.dumps(identity,sort_keys=True).encode()).hexdigest()
            self.disk=base/key
            if self.disk.resolve()!=self.disk:raise RuntimeError('Download cache entry follows a link')
            self.disk.mkdir(exist_ok=True)
            atomic_json(self.disk/'identity.json',identity)
    def _open(self,start,end):
        last=None
        headers={'Range':f'bytes={start}-{end}','User-Agent':'LocalGemmaStudio','Accept-Encoding':'identity'}
        if strong_etag(self.etag):headers['If-Range']=self.etag
        elif self.modified:headers['If-Range']=self.modified
        for attempt in range(3):
            try:return self.opener.open(urllib.request.Request(self.url,headers=headers),timeout=60)
            except Exception as e:last=e;time.sleep(attempt)
        raise last
    def _cached(self,start,length):
        if self.disk is None:return None
        block=self.disk/str(start);metadata=self.disk/(str(start)+'.json')
        if block.resolve()!=block or metadata.resolve()!=metadata:raise RuntimeError('Download block follows a link')
        try:
            if block.stat().st_size!=length or metadata.stat().st_size>4096:return None
            record=json.loads(metadata.read_text(encoding='utf-8'))
            with block.open('rb') as source:data=source.read(length+1)
            if len(data)==length and record=={'length':length,'sha256':hashlib.sha256(data).hexdigest()}:return data
        except (FileNotFoundError,ValueError,KeyError):pass
        return None
    def _save(self,start,data):
        if self.disk is None:return
        block=self.disk/str(start);metadata=self.disk/(str(start)+'.json')
        if block.resolve()!=block or metadata.resolve()!=metadata:raise RuntimeError('Download block follows a link')
        temporary=self.disk/(str(start)+'.'+uuid.uuid4().hex+'.part')
        try:
            with temporary.open('xb') as output:output.write(data);output.flush();os.fsync(output.fileno())
            os.replace(temporary,block)
            atomic_json(metadata,{'length':len(data),'sha256':hashlib.sha256(data).hexdigest()})
        finally:
            if temporary.exists():temporary.unlink()
    def _block(self,start,end):
        data=self._cached(start,end-start+1)
        if data is not None:return data
        with self._open(start,end) as response:
            if response.status!=206 or response.headers.get('Content-Range')!=f'bytes {start}-{end}/{self.size}':raise RuntimeError('Archive changed or invalid HTTP byte range response')
            if self.etag!=response.headers.get('ETag') or self.modified!=response.headers.get('Last-Modified'):raise RuntimeError('Archive identity changed during download; retry uses a separate cache')
            data=response.read(end-start+2)
        if len(data)!=end-start+1:raise RuntimeError('Incomplete HTTP byte range')
        self._save(start,data)
        return data
    def seekable(self):return True
    def readable(self):return True
    def tell(self):return self.pos
    def seek(self,offset,whence=0):
        if whence not in (0,1,2):raise ValueError('Invalid seek mode')
        position=offset if whence==0 else self.pos+offset if whence==1 else self.size+offset
        if position<0:raise ValueError('Negative ZIP position')
        self.pos=position;return self.pos
    def read(self,n=-1):
        if n<0:n=max(0,self.size-self.pos)
        n=min(n,max(0,self.size-self.pos))
        result=bytearray()
        while n>0:
            if not self.cache_start<=self.pos<self.cache_start+len(self.cache):
                start=self.pos//self.block_size*self.block_size;end=min(self.size-1,start+self.block_size-1)
                self.cache=self._block(start,end);self.cache_start=start
            offset=self.pos-self.cache_start;take=min(n,len(self.cache)-offset)
            result.extend(self.cache[offset:offset+take]);self.pos+=take;n-=take
        return bytes(result)

def relative_path(raw,backend):
    path=PurePosixPath(raw.replace('\\','/'))
    if path.is_absolute() or not path.parts or '..' in path.parts:raise ValueError('Unsafe package path')
    for part in path.parts:
        if re.search(r'[<>:"|?*\x00-\x1f]',part) or part.endswith((' ','.')) or re.fullmatch(r'(?i)(con|prn|aux|nul|com[1-9¹²³]|lpt[1-9¹²³])(?:\..*)?',part):raise ValueError('Unsafe Windows package path')
    if str(path)==backend+'-package.json':return path
    if len(path.parts)<4 or path.parts[:2]!=('lib','ollama') or not path.parts[2].lower().startswith(backend):raise ValueError('Package path outside backend')
    return path

def members(z,backend):
    seen=set()
    for entry in z.infolist():
        raw=entry.orig_filename.replace('\\','/');parts=PurePosixPath(raw).parts
        if entry.is_dir() or len(parts)<4 or parts[:2]!=('lib','ollama') or not parts[2].lower().startswith(backend):continue
        path=relative_path(raw,backend);key=str(path).casefold()
        if key in seen:raise ValueError('Duplicate package destination')
        seen.add(key);yield entry,path

def safe_path(base,path):
    base=base.resolve();value=base.joinpath(*path.parts)
    # Refuse alias/reparse parents before replacing any installed path.
    if value.resolve()!=value or not value.is_relative_to(base):raise RuntimeError('Package path follows a link or escapes its directory')
    return value

def install_target(root):
    root=root.resolve();target=root/'runtime/ollama'
    if target.resolve()!=target:raise RuntimeError('Installed engine directory follows a link')
    return target

def jobs_root(root):
    root=root.resolve();base=root/'data/package-transactions'
    if base.resolve()!=base or not base.resolve().is_relative_to(root):raise RuntimeError('Package journal escapes application root or follows a link')
    return base

def digest(path):
    if not path.exists():return None
    if not path.is_file() or path.is_symlink():raise RuntimeError('Package destination is not a regular file')
    value=hashlib.sha256()
    with path.open('rb') as source:
        for chunk in iter(lambda:source.read(1024*1024),b''):value.update(chunk)
    return value.hexdigest()

def atomic_json(path,value):
    temporary=path.with_name(path.name+'.'+uuid.uuid4().hex+'.tmp')
    try:
        with temporary.open('x',encoding='utf-8') as output:json.dump(value,output,ensure_ascii=False);output.flush();os.fsync(output.fileno())
        os.replace(temporary,path)
    finally:
        if temporary.exists():temporary.unlink()

def copy_synced(source,destination):
    destination.parent.mkdir(parents=True,exist_ok=True)
    with source.open('rb') as src,destination.open('xb') as out:shutil.copyfileobj(src,out,1024*1024);out.flush();os.fsync(out.fileno())

def remove_job(root,job):
    base=jobs_root(root).resolve();resolved=job.resolve()
    if resolved.parent!=base or resolved.name.lower()!=str(uuid.UUID(resolved.name)):raise RuntimeError('Unsafe package journal cleanup')
    # Keep the recovery decision until all staged files/backups have been removed.
    for child in resolved.iterdir():
        if child.name=='journal.json':continue
        checked=child.resolve()
        if checked!=child or not checked.is_relative_to(resolved):raise RuntimeError('Package cleanup follows a link')
        if child.is_dir():shutil.rmtree(checked)
        else:child.unlink()
    journal=resolved/'journal.json'
    if journal.exists():journal.unlink()
    resolved.rmdir()

def cleanup_finished(root,job):
    try:remove_job(root,job)
    except Exception as error:
        warning='Остались резервные копии установки; очистка не завершена: '+str(error)
        print(json.dumps({'status':'cleanup_pending','journal':str(job),'warning':warning},ensure_ascii=False),file=sys.stderr,flush=True)
        return warning
    return None

def rollback(root,job,plan):
    errors=[];backend=plan['backend'];target=install_target(root)
    for index,item in reversed(list(enumerate(plan['files']))):
        try:
            path=safe_path(target,relative_path(item['path'],backend));current=digest(path)
            if current==item['before']:continue
            if current!=item['after']:raise RuntimeError('File changed outside this installation; recovery did not overwrite it')
            if item['before'] is None:path.unlink()
            else:
                backup=safe_path(job,PurePosixPath('old')/str(index))
                if digest(backup)!=item['before']:raise RuntimeError('Package backup is missing or damaged')
                temporary=path.with_name(path.name+'.'+uuid.uuid4().hex+'.restore')
                try:copy_synced(backup,temporary);os.replace(temporary,path)
                finally:
                    if temporary.exists():temporary.unlink()
            if digest(path)!=item['before']:raise RuntimeError('Package rollback verification failed')
        except Exception as error:errors.append({'path':item.get('path'),'error':str(error)})
    if errors:
        plan['status']='recovery_required';plan['errors']=errors;atomic_json(job/'journal.json',plan)
        raise RuntimeError('Package recovery requires attention; files and backups retained: '+json.dumps(errors,ensure_ascii=False))
    plan['status']='rolled_back';atomic_json(job/'journal.json',plan)
    return cleanup_finished(root,job)

def recover(root):
    root=root.resolve();base=jobs_root(root)
    if not base.exists():return
    for job in base.iterdir():
        # Only this installer's UUID directories are owned by recovery.
        try:valid=job.name.lower()==str(uuid.UUID(job.name))
        except ValueError:continue
        if not valid:continue
        if job.resolve().parent!=base.resolve() or job.is_symlink():raise RuntimeError('Package journal is a link')
        journal=job/'journal.json'
        if not journal.exists():
            if (job/'old').exists():raise RuntimeError('Package journal is missing; backups retained for inspection')
            remove_job(root,job);continue # extraction staging never touched installed files
        journal=safe_path(job,PurePosixPath('journal.json'))
        plan=json.loads(journal.read_text(encoding='utf-8'))
        if plan.get('version')!=1 or plan.get('backend') not in ('cuda','vulkan','rocm'):raise RuntimeError('Unknown package journal; preserved')
        if plan['status'] in ('committed','rolled_back'):cleanup_finished(root,job)
        else:rollback(root,job,plan)

def publish(root,job,backend,paths,checkpoint=None):
    target=install_target(root);files=[]
    for index,relative in enumerate(paths):
        destination=safe_path(target,relative_path(str(relative),backend));before=digest(destination)
        if before is not None:
            copy_synced(destination,job/'old'/str(index))
            if digest(job/'old'/str(index))!=before or digest(destination)!=before:raise RuntimeError('Installed file changed during backup')
        files.append({'path':str(relative),'before':before,'after':digest(safe_path(job/'new',relative))})
        if files[-1]['after'] is None:raise RuntimeError('Staged package file missing')
    plan={'version':1,'backend':backend,'status':'prepared','files':files};atomic_json(job/'journal.json',plan)
    try:
        for index,item in enumerate(files):
            relative=relative_path(item['path'],backend);destination=safe_path(target,relative)
            if digest(destination)!=item['before']:raise RuntimeError('Installed file changed before publication')
            destination.parent.mkdir(parents=True,exist_ok=True)
            os.replace(safe_path(job/'new',relative),destination)
            if digest(destination)!=item['after']:raise RuntimeError('Package publication verification failed')
            if checkpoint:checkpoint(index)
        plan['status']='committed';atomic_json(job/'journal.json',plan)
    except Exception as error:
        try:rollback(root,job,plan)
        except Exception as recovery:raise RuntimeError(str(error)+'; '+str(recovery)) from error
        raise
    return cleanup_finished(root,job)

def install(root,backend,version,list_only=False):
    if not re.fullmatch(r'\d+\.\d+\.\d+(?:[-.][A-Za-z0-9]+)*',version):raise ValueError('Invalid Ollama version')
    if backend not in ['cuda','vulkan','rocm']:raise ValueError('Unknown backend')
    root=root.resolve();recover(root)
    binary=install_target(root)/'ollama.exe'
    with binary.open('rb') as file:
        file.seek(0x3c);offset=int.from_bytes(file.read(4),'little');file.seek(offset+4);machine=int.from_bytes(file.read(2),'little')
    arch={0x8664:'amd64',0xaa64:'arm64'}.get(machine)
    if not arch:raise RuntimeError('Unsupported engine binary architecture')
    asset='ollama-windows-'+arch+('-rocm' if backend=='rocm' else '')+'.zip'
    url=f'https://github.com/ollama/ollama/releases/download/v{version}/{asset}'
    print(json.dumps({'status':'index','backend':backend,'version':version}),flush=True)
    with RangeZip(url,root/'data/package-downloads') as remote,zipfile.ZipFile(remote) as archive:
        selected=list(members(archive,backend))
        if not selected:raise RuntimeError('Matching GPU libraries are absent from the release archive')
        total=sum(e.file_size for e,_ in selected)
        print(json.dumps({'status':'files','bytes':total,'files':[str(p) for _,p in selected]}),flush=True)
        if list_only:return
        if shutil.disk_usage(root).free<total*2+256*1024*1024:raise RuntimeError('Not enough free space on the application drive')
        job=jobs_root(root)/str(uuid.uuid4());stage=job/'new';stage.mkdir(parents=True,exist_ok=False)
        try:
            done=0
            for entry,path in selected:
                destination=safe_path(stage,path);destination.parent.mkdir(parents=True,exist_ok=True)
                with archive.open(entry) as source,destination.open('xb') as output:shutil.copyfileobj(source,output,1024*1024);output.flush();os.fsync(output.fileno())
                if destination.stat().st_size!=entry.file_size:raise RuntimeError('Incomplete library extraction')
                done+=entry.file_size;print(json.dumps({'status':'extract','file':str(path),'completed':done,'total':total}),flush=True)
            marker=PurePosixPath(backend+'-package.json')
            atomic_json(stage/str(marker),{'backend':backend,'version':version,'files':[str(p) for _,p in selected]})
            publish(root,job,backend,[path for _,path in selected]+[marker])
            print(json.dumps({'status':'ready','backend':backend,'bytes':total}),flush=True)
        finally:
            if job.exists() and not (job/'journal.json').exists():remove_job(root,job)

if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('--root',type=Path,required=True);p.add_argument('--backend',choices=['cuda','vulkan','rocm']);p.add_argument('--version');p.add_argument('--list',action='store_true');p.add_argument('--recover',action='store_true');a=p.parse_args()
    if a.recover:recover(a.root.resolve())
    else:
        if not a.backend or not a.version:p.error('--backend and --version are required for installation')
        install(a.root.resolve(),a.backend,a.version,a.list)
