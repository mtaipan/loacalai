import contextlib,json,sys
from pathlib import Path
sys.stdout.reconfigure(encoding='utf-8');sys.stderr.reconfigure(encoding='utf-8')
try:
    import document_tools
    request=json.loads(Path(sys.argv[1]).read_text(encoding='utf-8'))
    with contextlib.redirect_stdout(sys.stderr):result=document_tools.process(request)
    output=json.dumps(result,ensure_ascii=False)
    if len(output.encode('utf-8'))>(1000000 if request.get('_result_path') else 200000):raise ValueError('Результат слишком большой. Уменьшите диапазон страниц или разделите документ.')
    if request.get('_result_path'):
        Path(request['_result_path']).write_text(output,encoding='utf-8');print('{"result_file":true}')
    else:print(output)
except Exception as error:
    print(json.dumps({'error':str(error)},ensure_ascii=False));sys.exit(1)
