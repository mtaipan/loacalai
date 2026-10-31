from pathlib import Path
import zipfile,hashlib,json,subprocess,os
r=Path(__file__).resolve().parent.parent;dist=r/'dist';dist.mkdir(exist_ok=True)
(r/'build-tools').mkdir(exist_ok=True)
base=[r/n for n in ['.gitignore','.gitattributes','Cargo.toml','Cargo.lock','README.md','START-HERE.md','MODELS.md','WEB-AND-THINKING.md','LICENSE','RELEASE-0.4.md'] if (r/n).exists()]
source=base+[p for folder in ['src','web','scripts'] for p in (r/folder).rglob('*') if p.is_file() and '__pycache__' not in p.parts and p.suffix not in ['.exe']]
source += [p for p in (r/'runtime').iterdir() if p.is_file() and p.suffix in ['.py','.ps1','.txt']]
source += list(r.glob('*.command'))+list(r.glob('*.bat'))
def archive(path,files,prefix=''):
 with zipfile.ZipFile(path,'w',compression=zipfile.ZIP_DEFLATED,compresslevel=9) as z:
  for p in sorted(set(files)):
   i=zipfile.ZipInfo(prefix+p.relative_to(r).as_posix());i.create_system=3
   i.external_attr=(0o100755 if p.suffix in ['.command','.sh'] else 0o100644)<<16;i.compress_type=zipfile.ZIP_DEFLATED
   z.writestr(i,p.read_bytes().replace(b'\r\n',b'\n') if p.suffix in ['.command','.sh'] else p.read_bytes())
 with zipfile.ZipFile(path) as z: assert z.testzip() is None
archive(dist/'LocalGemmaStudio-macOS-source.zip',source,'LocalGemmaStudio/')
payload=[r/'LocalGemmaStudio.exe',r/'LICENSE',r/'src/QWEN3-TEMPLATE-LICENSE.txt',r/'RELEASE-0.4.md',r/'WEB-AND-THINKING.md',r/'scripts/provision-windows.ps1']+[p for p in (r/'runtime').iterdir() if p.is_file() and p.suffix in ['.py','.ps1','.txt']]
archive(r/'build-tools/payload.zip',payload)
csc=Path(os.environ.get('SystemRoot','C:/Windows'))/'Microsoft.NET/Framework64/v4.0.30319/csc.exe'
subprocess.run([str(csc),'/nologo','/target:winexe','/platform:x64','/optimize+',f'/out:{dist}/LocalGemmaStudio-Setup.exe',f'/resource:{r}/build-tools/payload.zip,payload.zip','/r:System.Windows.Forms.dll','/r:System.Drawing.dll','/r:System.IO.Compression.dll','/r:System.IO.Compression.FileSystem.dll',str(r/'scripts/Setup.cs')],check=True)
report={}
for p in [dist/'LocalGemmaStudio-Setup.exe',dist/'LocalGemmaStudio-macOS-source.zip',r/'LocalGemmaStudio.exe']:
 report[p.name]={'bytes':p.stat().st_size,'sha256':hashlib.sha256(p.read_bytes()).hexdigest()}
(dist/'SHA256.json').write_text(json.dumps(report,indent=2)+'\n')
print(json.dumps(report,indent=2))
