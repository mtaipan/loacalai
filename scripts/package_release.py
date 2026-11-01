from pathlib import Path
import zipfile,hashlib,json,subprocess,os
r=Path(__file__).resolve().parent.parent;dist=r/'dist';dist.mkdir(exist_ok=True)
(r/'build-tools').mkdir(exist_ok=True)
base=[r/n for n in ['.gitignore','.gitattributes','Cargo.toml','Cargo.lock','README.md','START-HERE.md','MODELS.md','WEB-AND-THINKING.md','LICENSE','RELEASE-0.4.md','RELEASE-0.13.md','RELEASE-0.14.md','RELEASE-0.15.md','RELEASE-0.16.md','REVIEW-2026-11-01.md'] if (r/n).exists()]
source=base+[p for folder in ['src','web','scripts'] for p in (r/folder).rglob('*') if p.is_file() and '__pycache__' not in p.parts and p.suffix not in ['.exe']]
source += [p for p in (r/'runtime').iterdir() if p.is_file() and p.suffix in ['.py','.ps1','.txt']]
source += list(r.glob('*.command'))+list(r.glob('*.bat'))
def archive(path,files,prefix=''):
 with zipfile.ZipFile(path,'w',compression=zipfile.ZIP_DEFLATED,compresslevel=6) as z:
  for p in sorted(set(files)):
   i=zipfile.ZipInfo(prefix+p.relative_to(r).as_posix());i.create_system=3
   i.external_attr=(0o100755 if p.suffix in ['.command','.sh'] else 0o100644)<<16;i.compress_type=zipfile.ZIP_DEFLATED
   z.writestr(i,p.read_bytes().replace(b'\r\n',b'\n') if p.suffix in ['.command','.sh'] else p.read_bytes())
 with zipfile.ZipFile(path) as z: assert z.testzip() is None
archive(dist/'LocalGemmaStudio-macOS-source.zip',source,'LocalGemmaStudio/')
payload=[r/'LocalGemmaStudio.exe',r/'LICENSE',r/'src/QWEN3-TEMPLATE-LICENSE.txt',r/'RELEASE-0.4.md',r/'WEB-AND-THINKING.md',r/'scripts/provision-windows.ps1']+[p for p in (r/'runtime').iterdir() if p.is_file() and p.suffix in ['.py','.ps1','.txt']]
payload += [r/'scripts/ensure-webview.ps1',r/'scripts/install_models.py',r/'scripts/uninstall.ps1',r/'RELEASE-0.13.md',r/'RELEASE-0.14.md',r/'RELEASE-0.15.md',r/'RELEASE-0.16.md',r/'REVIEW-2026-11-01.md']
payload += [p for p in (r/'runtime/python').rglob('*') if p.is_file() and '__pycache__' not in p.parts and not p.name.endswith(('.pyc','.pyo'))]
payload += [r/'runtime/ollama/ollama.exe']+[p for p in (r/'runtime/ollama/lib/ollama').iterdir() if p.is_file()]
webview=r/'build-tools/MicrosoftEdgeWebView2RuntimeInstallerX64.exe'
if not webview.is_file():
 import urllib.request
 urllib.request.urlretrieve('https://go.microsoft.com/fwlink/p/?LinkId=2124701', webview)
 signature=subprocess.run(['powershell.exe','-NoProfile','-Command',"if((Get-AuthenticodeSignature -LiteralPath $env:LOCAL_GEMMA_WEBVIEW_COMPONENT).Status -ne 'Valid'){exit 1}"],env={**os.environ,'LOCAL_GEMMA_WEBVIEW_COMPONENT':str(webview)},check=False)
 assert signature.returncode==0, 'Offline WebView2 signature verification failed'
# The component is placed under runtime in the payload, outside source archives.
archive(r/'build-tools/payload.zip',payload)
with zipfile.ZipFile(r/'build-tools/payload.zip','a',compression=zipfile.ZIP_STORED) as z:
 z.write(webview,'runtime/'+webview.name)

csc=Path(os.environ.get('SystemRoot','C:/Windows'))/'Microsoft.NET/Framework64/v4.0.30319/csc.exe'
subprocess.run([str(csc),'/nologo','/target:winexe','/platform:x64','/optimize+',f'/out:{dist}/LocalGemmaStudio-Setup.exe',f'/resource:{r}/build-tools/payload.zip,payload.zip',f'/resource:{r}/src/model-catalog.json,catalog.json','/r:System.Web.Extensions.dll','/r:System.Core.dll','/r:System.Windows.Forms.dll','/r:System.Drawing.dll','/r:System.IO.Compression.dll','/r:System.IO.Compression.FileSystem.dll',str(r/'scripts/Setup.cs')],check=True)
report={}
for p in [dist/'LocalGemmaStudio-Setup.exe',dist/'LocalGemmaStudio-macOS-source.zip',r/'LocalGemmaStudio.exe']:
 report[p.name]={'bytes':p.stat().st_size,'sha256':hashlib.sha256(p.read_bytes()).hexdigest()}
(dist/'SHA256.json').write_text(json.dumps(report,indent=2)+'\n')
print(json.dumps(report,indent=2))
