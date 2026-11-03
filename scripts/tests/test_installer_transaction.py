"""Compile the real installer transaction, invoke only isolated tiny fixture roots.
No GUI, provisioning, launch, model download or installed application is invoked.
"""
from pathlib import Path
import hashlib,json,os,queue,struct,subprocess,threading,time,uuid,zipfile
project=Path(__file__).resolve().parents[2]
area=project/'build-tools'/('installer-transaction-'+uuid.uuid4().hex[:8]);area.mkdir(parents=True)
exe=area/'fixture.exe';csc=Path(os.environ.get('SystemRoot','C:/Windows'))/'Microsoft.NET/Framework64/v4.0.30319/csc.exe'
subprocess.run([str(csc),'/nologo','/target:exe','/main:InstallerFixture','/out:'+str(exe),'/r:System.Web.Extensions.dll','/r:System.Core.dll','/r:System.Windows.Forms.dll','/r:System.Drawing.dll','/r:System.IO.Compression.dll','/r:System.IO.Compression.FileSystem.dll',str(project/'scripts/Setup.cs'),str(project/'scripts/tests/installer_fixture.cs')],check=True)
payload=area/'payload.zip'
with zipfile.ZipFile(payload,'w') as z:
 for path,text in {'LocalGemmaStudio.exe':'NEW_APP','runtime/worker.txt':'NEW_WORKER','scripts/provision-windows.ps1':'NO_EXECUTION'}.items():z.writestr(path,text)
def line(process):
 channel=queue.Queue();threading.Thread(target=lambda:channel.put(process.stdout.readline()),daemon=True).start()
 return channel.get(timeout=10).strip()
def old(name):
 root=area/name;(root/'runtime/optional').mkdir(parents=True);(root/'data/models').mkdir(parents=True)
 for path,text in {'LocalGemmaStudio.exe':'OLD_APP','runtime/worker.txt':'OLD_WORKER','runtime/obsolete.txt':'OLD_ONLY','runtime/optional/module.bin':'OPTIONAL','data/models/weights.bin':'MODEL_DO_NOT_COPY','data/settings.json':'USER_SETTINGS','private.txt':'PRIVATE'}.items():(root/path).write_text(text)
 (root/'install-manifest.txt').write_text('LocalGemmaStudio.exe\nruntime/worker.txt\nruntime/obsolete.txt\n')
 return root
def run(root,point='',op='apply',archive=payload,expected=0):
 result=subprocess.run([str(exe),op,str(root),str(archive),point],capture_output=True,text=True,encoding="utf-8")
 assert result.returncode==expected,(root,point,result.returncode,result.stdout,result.stderr)
 return result
def check(root,new=False):
 assert (root/'LocalGemmaStudio.exe').read_text()==('NEW_APP' if new else 'OLD_APP')
 assert (root/'runtime/worker.txt').read_text()==('NEW_WORKER' if new else 'OLD_WORKER')
 for path,text in {'runtime/optional/module.bin':'OPTIONAL','data/models/weights.bin':'MODEL_DO_NOT_COPY','data/settings.json':'USER_SETTINGS','private.txt':'PRIVATE'}.items():assert (root/path).read_text()==text,(path,root)
 assert (root/'runtime/obsolete.txt').exists()!=new
# Crash/restart before/after every significant same-volume rename/carry/commit.
for point in ['after_extract','after_verify','before_old_move','after_old_move','after_carry_0','after_carry_1','after_carry_2','after_carry','after_publish','after_commit']:
 root=old(point);run(root,point,expected=71);run(root,op='recover');check(root,new=point in ['after_publish','after_commit'])
 assert not (area/('.'+root.name+'.update.json')).exists()
print('PASS crash/restart restores old complete root before publication; full publication survives interrupted commit journal')
for point in ['recovery_after_carry_0','recovery_after_carry_1','recovery_after_carry_2','recovery_after_restore']:
 root=old(point);run(root,'after_carry',expected=71);run(root,point,op='recover',expected=72);run(root,op='recover');check(root)
root=old('first-launch-data');run(root,'after_publish',expected=71);(root/'data/settings.json').write_text('FIRST_LAUNCH_CHANGED');(root/'data/chat.json').write_text('NEW_CHAT');run(root,op='recover');assert (root/'data/settings.json').read_text()=='FIRST_LAUNCH_CHANGED' and (root/'data/chat.json').read_text()=='NEW_CHAT' and (root/'LocalGemmaStudio.exe').read_text()=='NEW_APP'
root=old('tamper-before-publish');run(root,'tamper',expected=3);check(root)
root=old('legacy-startup-conflict');run(root,'after_old_move',expected=71);journal=area/('.'+root.name+'.update.json');state=json.loads(journal.read_text(encoding='utf-8'));(root/'data').mkdir(parents=True);(root/'data/unexpected.txt').write_text('LEGACY_NEW_DATA');run(root,op='recover',expected=3)
assert (root/'data/unexpected.txt').read_text()=='LEGACY_NEW_DATA' and (Path(state['Backup'])/'data/settings.json').read_text()=='USER_SETTINGS' and journal.exists()
print('PASS recovery itself resumes after each carry/restore; published first-launch data is never rolled back; hash conflict restores old root')
root=old('success');model_id=(root/'data/models/weights.bin').stat().st_ino;run(root);check(root,new=True);assert (root/'data/models/weights.bin').stat().st_ino==model_id
manifest=json.loads((root/'install-manifest.json').read_text(encoding='utf-8'))
assert Path(manifest['previous_backup']).is_dir()
for item in manifest['files']:
 data=(root/item['Path']).read_bytes();assert len(data)==item['Length'] and hashlib.sha256(data).hexdigest()==item['Sha256']
assert (Path(manifest['previous_backup'])/'LocalGemmaStudio.exe').read_text()=='OLD_APP'
print('PASS successful version uses verified SHA256 manifest, retains previous version and carries settings/models/optional modules')
root=old('verification-failure');run(root,'verify_fail',expected=3);check(root)
root=old('verification-modification');run(root,'verify_modify',expected=3);check(root)
root=old('locked-app');holder=subprocess.Popen([str(exe),'lease',str(root)],stdout=subprocess.PIPE,text=True)
try:
 assert line(holder)=='LOCKED';result=run(root,expected=3);assert 'IOException' in result.stderr;check(root)
finally:holder.kill();holder.wait()
# FileShare.None simulates a busy payload executable independently of the app lease.
lock_script=area/'lock.ps1';lock_script.write_text("$f=[IO.File]::Open($env:INSTALLER_FIXTURE_LOCK,[IO.FileMode]::Open,[IO.FileAccess]::Read,[IO.FileShare]::None);[Console]::WriteLine('LOCKED');Start-Sleep -Seconds 30;$f.Dispose()")
root=old('locked-file');holder=subprocess.Popen(['powershell.exe','-NoProfile','-File',str(lock_script)],env={**os.environ,'INSTALLER_FIXTURE_LOCK':str(root/'LocalGemmaStudio.exe')},stdout=subprocess.PIPE,text=True)
try:
 assert line(holder)=='LOCKED';run(root,expected=3)
finally:holder.kill();holder.wait()
check(root)
print('PASS verification failure and busy app/file preserve previous version without terminating the owner')
for name,entries in [('escape',{'../outside':'BAD'}),('data',{'data/settings.json':'BAD'}),('duplicate',{'runtime\\worker.txt':'a','runtime/worker.txt':'b'})]:
 bad=area/(name+'.zip')
 with zipfile.ZipFile(bad,'w') as z:
  for path,text in entries.items():z.writestr(path,text)
 root=old(name);run(root,archive=bad,expected=3);check(root)
# A valid ZIP64 directory declares a component larger than the fixture volume;
# admission must reject before expanding or changing the installed release.
name=b'huge.bin';extra=struct.pack('<HHQ',1,8,1<<54)
local=struct.pack('<IHHHHHIIIHH',0x04034b50,45,0,0,0,0,0,0,0,len(name),0)+name
central=struct.pack('<IHHHHHHIIIHHHHHII',0x02014b50,45,45,0,0,0,0,0,0,0xffffffff,len(name),len(extra),0,0,0,0,0)+name+extra
disk_full=area/'disk-full.zip';disk_full.write_bytes(local+central+struct.pack('<IHHHHIIH',0x06054b50,0,0,1,1,len(central),len(local),0))
root=old('disk-full');result=run(root,archive=disk_full,expected=3);assert '\u041d\u0435\u0434\u043e\u0441\u0442\u0430\u0442\u043e\u0447\u043d\u043e \u043c\u0435\u0441\u0442\u0430' in result.stderr,result.stderr;check(root)
assert not (area/'outside').exists()
root=area/'new-install';run(root);assert (root/'LocalGemmaStudio.exe').read_text()=='NEW_APP' and (root/'data/instance.lock').exists()
root=area/'new-crash';run(root,'after_publish',expected=71);run(root,op='recover');assert (root/'LocalGemmaStudio.exe').read_text()=='NEW_APP'
print('PASS traversal/data/duplicate archive rejection and new-install recovery; no live installer or app invoked')
# Preserved directory junction is moved as a link; external target is untouched.
external=area/'external-models';external.mkdir();(external/'sentinel').write_text('EXTERNAL')
root=old('junction');subprocess.run(['powershell.exe','-NoProfile','-Command','New-Item -ItemType Junction -Path $env:INSTALLER_LINK -Target $env:INSTALLER_TARGET | Out-Null'],env={**os.environ,'INSTALLER_LINK':str(root/'model-junction'),'INSTALLER_TARGET':str(external)},check=True)
run(root,'after_carry',expected=71);run(root,op='recover');check(root);assert (root/'model-junction/sentinel').read_text()=='EXTERNAL' and (external/'sentinel').read_text()=='EXTERNAL'
run(root);check(root,new=True);assert (root/'model-junction/sentinel').read_text()=='EXTERNAL' and (external/'sentinel').read_text()=='EXTERNAL'
print('PASS carried junction rollback/publication never traverses or deletes external model target')
# Staging never takes the publication mutex. Publication waits for startup admission.
root=old('guard-publication');holder=subprocess.Popen([str(exe),'guard',str(root)],stdout=subprocess.PIPE,text=True,encoding='utf-8');publish=None
try:
 assert line(holder)=='LOCKED'
 publish=subprocess.Popen([str(exe),'apply',str(root),str(payload),'verify_signal'],stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True,encoding='utf-8')
 assert line(publish)=='STAGED';assert publish.poll() is None;check(root)
 holder.kill();holder.wait();out,err=publish.communicate(timeout=10);assert publish.returncode==0,(out,err)
finally:
 if holder.poll() is None:holder.kill();holder.wait()
 if publish is not None and publish.poll() is None:publish.kill();publish.wait()
check(root,new=True)
root=old('guard-staging');staging=subprocess.Popen([str(exe),'apply',str(root),str(payload),'verify_wait'],stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True,encoding='utf-8');holder=None
try:
 assert line(staging)=='VERIFY_WAIT'
 holder=subprocess.Popen([str(exe),'guard',str(root)],stdout=subprocess.PIPE,text=True,encoding='utf-8');assert line(holder)=='LOCKED';check(root)
finally:
 staging.kill();staging.wait()
 if holder is not None:holder.kill();holder.wait()
run(root,op='recover');check(root)
key=r'C:\Guard Fixture\app';normalized=key.replace('/','\\').upper();expected='Global\\LocalGemmaStudio.Install.'+hashlib.sha256(normalized.encode()).hexdigest()
assert run(Path(key),op='name').stdout.strip()==expected
print('PASS publication mutex waits for startup admission, staging does not hold it, abandoned guard recovers, UTF-8 SHA256 identity matches contract')

# Optional fresh Rust binary: only startup admission runs; no app runtime/data
# may be created in either blocked case, and no inference endpoint is contacted.
app_exe=os.environ.get('LOCAL_GEMMA_TEST_EXE')
if app_exe:
 app_exe=str(Path(app_exe).resolve());assert Path(app_exe).is_file()
 def blocked_start(root):
  result=subprocess.run([app_exe,'--server'],cwd=area,env={**os.environ,'LOCAL_GEMMA_HOME':str(root),'LOCAL_GEMMA_OLLAMA':'http://127.0.0.1:1'},capture_output=True,timeout=10)
  assert result.returncode!=0,(result.stdout,result.stderr)
  assert not (root/'data').exists(),root
 root=area/'rust-pending';stage=area/('.'+root.name+'.stage-'+uuid.uuid4().hex);backup=area/('.'+root.name+'.backup-'+uuid.uuid4().hex);stage.mkdir();backup.mkdir()
 (area/('.'+root.name+'.update.json')).write_text(json.dumps({'Version':1,'Root':str(root),'Stage':str(stage),'Backup':str(backup),'Phase':'old_moved','HadRoot':True,'Carry':['data']}),encoding='utf-8')
 blocked_start(root);assert not root.exists()
 for name in ['rust-mutex','Приложение-ß']:
  root=area/name;holder=subprocess.Popen([str(exe),'guard',str(root)],stdout=subprocess.PIPE,text=True,encoding='utf-8')
  try:
   assert line(holder)=='LOCKED';blocked_start(root);assert not root.exists()
  finally:holder.kill();holder.wait()
 print('PASS actual Rust startup obeys C# ASCII/Unicode publication mutex and pending recovery before creating any data/root')
else:print('SKIP actual Rust startup interoperability: set LOCAL_GEMMA_TEST_EXE to a fresh binary')

print(area)
