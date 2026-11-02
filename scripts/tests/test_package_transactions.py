"""GPU publication/recovery uses tiny own files and ZIPs; never downloads or runs an engine."""
from pathlib import Path,PurePosixPath
import contextlib,ctypes,io,json,os,shutil,subprocess,sys,tempfile,uuid,zipfile
project=Path(__file__).resolve().parents[2];sys.path.insert(0,str(project/'runtime'));import compute_packages as package
root=Path(tempfile.mkdtemp(prefix='package-transactions-',dir=project/'build-tools')).resolve();assert root.is_relative_to(project.resolve())
paths=[PurePosixPath('lib/ollama/cuda_v12/one.dll'),PurePosixPath('lib/ollama/cuda_v12/two.dll'),PurePosixPath('cuda-package.json')]
def fixture(name):
    case=root/name;target=case/'runtime/ollama';target.joinpath(*paths[0].parts).parent.mkdir(parents=True)
    target.joinpath(*paths[0].parts).write_bytes(b'OLD_ONE');(target/'cuda-package.json').write_bytes(b'OLD_MARKER')
    sibling=target/'lib/ollama/vulkan/keep.dll';sibling.parent.mkdir(parents=True);sibling.write_bytes(b'UNRELATED_BACKEND')
    job=package.jobs_root(case)/str(uuid.uuid4());(job/'new').mkdir(parents=True)
    for path,data in zip(paths,[b'NEW_ONE',b'NEW_TWO',b'NEW_MARKER']):
        destination=job/'new'/str(path);destination.parent.mkdir(parents=True,exist_ok=True);destination.write_bytes(data)
    return case,target,job
def old(target):
    assert target.joinpath(*paths[0].parts).read_bytes()==b'OLD_ONE'
    assert not target.joinpath(*paths[1].parts).exists()
    assert (target/'cuda-package.json').read_bytes()==b'OLD_MARKER'
    assert (target/'lib/ollama/vulkan/keep.dll').read_bytes()==b'UNRELATED_BACKEND'
try:
    case,target,job=fixture('success');package.publish(case,job,'cuda',paths)
    assert not job.exists() and target.joinpath(*paths[1].parts).read_bytes()==b'NEW_TWO'
    assert (target/'cuda-package.json').read_bytes()==b'NEW_MARKER'
    case,target,job=fixture('failure')
    def failed(index):
        if index==0:raise OSError('OWN_FIXTURE_IO_FAILURE')
    try:package.publish(case,job,'cuda',paths,failed);raise AssertionError('Expected failure')
    except OSError:pass
    old(target);assert not job.exists()
    print('PASS verified package publication and rollback restore old files/marker and remove newly created files')
    if os.name=='nt':
        case,target,job=fixture('locked-marker');kernel=ctypes.WinDLL('kernel32',use_last_error=True)
        kernel.CreateFileW.argtypes=[ctypes.c_wchar_p,ctypes.c_uint32,ctypes.c_uint32,ctypes.c_void_p,ctypes.c_uint32,ctypes.c_uint32,ctypes.c_void_p];kernel.CreateFileW.restype=ctypes.c_void_p
        kernel.CloseHandle.argtypes=[ctypes.c_void_p];kernel.CloseHandle.restype=ctypes.c_int
        handle=kernel.CreateFileW(str(target/'cuda-package.json'),0x80000000,1,None,3,0x80,None)
        assert handle not in (None,ctypes.c_void_p(-1).value)
        try:
            try:package.publish(case,job,'cuda',paths);raise AssertionError('Marker sharing lock ignored')
            except OSError:pass
            old(target);assert not job.exists()
        finally:assert kernel.CloseHandle(handle)
        print('PASS real Windows marker replacement failure rolls back previously replaced DLLs')
    case,target,job=fixture('crash')
    code='import sys,os;from pathlib import Path,PurePosixPath;sys.path.insert(0,'+repr(str(project/'runtime'))+');import compute_packages as p;p.publish(Path('+repr(str(case))+'),Path('+repr(str(job))+'),"cuda",[PurePosixPath(v) for v in '+repr([str(path) for path in paths])+'],checkpoint=lambda index:os._exit(7) if index==0 else None)'
    result=subprocess.run([sys.executable,'-c',code],capture_output=True);assert result.returncode==7,result.stderr
    assert target.joinpath(*paths[0].parts).read_bytes()==b'NEW_ONE' and (job/'journal.json').exists()
    package.recover(case);old(target);assert not job.exists()
    print('PASS a separately terminated process leaves a durable journal; independent recovery restores the old package')
    case,target,job=fixture('committed-cleanup');original_cleanup=package.remove_job
    def failed_cleanup(root,job):raise OSError('OWN_CLEANUP_INTERRUPTION')
    package.remove_job=failed_cleanup
    try:
        with contextlib.redirect_stderr(io.StringIO()):warning=package.publish(case,job,'cuda',paths)
        assert warning and target.joinpath(*paths[0].parts).read_bytes()==b'NEW_ONE'
    finally:package.remove_job=original_cleanup
    assert json.loads((job/'journal.json').read_bytes())['status']=='committed'
    package.recover(case);assert not job.exists() and target.joinpath(*paths[0].parts).read_bytes()==b'NEW_ONE'
    case,target,job=fixture('unknown-version');package.atomic_json(job/'journal.json',{'version':999,'backend':'cuda','status':'prepared','files':[]})
    try:package.recover(case);raise AssertionError('Unknown version accepted')
    except RuntimeError:pass
    assert job.exists();old(target)
    missing,target,job=fixture('missing-journal');(job/'old').mkdir();(job/'old/0').write_bytes(b'RETAIN_THIS_BACKUP')
    try:package.recover(missing);raise AssertionError('Missing journal accepted')
    except RuntimeError:pass
    assert (job/'old/0').read_bytes()==b'RETAIN_THIS_BACKUP';old(target)
    print('PASS committed cleanup preserves the new installation; unknown journals remain untouched')
    case,target,job=fixture('conflict')
    def conflict(index):
        if index==0:target.joinpath(*paths[0].parts).write_bytes(b'OWN_EXTERNAL_CHANGE');raise OSError('OWN_FAILURE')
    try:package.publish(case,job,'cuda',paths,conflict);raise AssertionError('Expected conflict')
    except RuntimeError as error:assert 'recovery' in str(error)
    assert target.joinpath(*paths[0].parts).read_bytes()==b'OWN_EXTERNAL_CHANGE'
    assert json.loads((job/'journal.json').read_bytes())['status']=='recovery_required'
    try:package.recover(case);raise AssertionError('Expected preserved conflict')
    except RuntimeError:pass
    assert (job/'old/0').read_bytes()==b'OLD_ONE'
    target.joinpath(*paths[0].parts).write_bytes(b'NEW_ONE');package.recover(case);old(target)
    print('PASS recovery refuses to overwrite external changes and retains independently verified backups')
    for unsafe in ['lib/ollama/cuda_v12/../../escape','lib/ollama/cuda_v12/C:/escape','lib/ollama/cuda_v12/NUL.dll','lib/ollama/cuda_v12/trailing.','lib/ollama/cuda_v12/COM¹.dll','lib/ollama/cuda_v12/a|b.dll','/lib/ollama/cuda_v12/one.dll']:
        try:package.relative_path(unsafe,'cuda');raise AssertionError(unsafe)
        except ValueError:pass
    archive=io.BytesIO()
    with zipfile.ZipFile(archive,'w') as output:
        output.writestr('lib/ollama/cuda_v12/one.dll',b'FIRST');output.writestr('lib/ollama/CUDA_v12/ONE.dll',b'SECOND')
    with zipfile.ZipFile(io.BytesIO(archive.getvalue())) as zipped:
        try:list(package.members(zipped,'cuda'));raise AssertionError('Duplicate destination accepted')
        except ValueError:pass
    print('PASS Windows drive/device/path aliases and case-insensitive duplicate ZIP destinations are rejected')
    if os.name=='nt':
        case,target,job=fixture('alias');outside=case/'outside';outside.mkdir();(outside/'source.dll').write_bytes(b'OUTSIDE_PACKAGE')
        alias=target/'lib/ollama/cuda_alias'
        subprocess.run(['powershell.exe','-NoProfile','-Command','New-Item -ItemType Junction -Path $env:TEST_ALIAS -Target $env:TEST_TARGET | Out-Null'],env={**os.environ,'TEST_ALIAS':str(alias),'TEST_TARGET':str(outside)},check=True)
        relative=PurePosixPath('lib/ollama/cuda_alias/source.dll');staged=job/'new'/str(relative);staged.parent.mkdir(parents=True);staged.write_bytes(b'BAD_REPLACEMENT')
        try:
            try:package.publish(case,job,'cuda',[relative]);raise AssertionError('Parent junction accepted')
            except RuntimeError:pass
            assert (outside/'source.dll').read_bytes()==b'OUTSIDE_PACKAGE';package.recover(case)
        finally:alias.rmdir()
        print('PASS a real Windows parent junction cannot redirect package publication')
    case,target,job=fixture('extract');package.remove_job(case,job)
    binary=bytearray(128);binary[0x3c:0x40]=(64).to_bytes(4,'little');binary[68:70]=(0x8664).to_bytes(2,'little');(target/'ollama.exe').write_bytes(binary)
    archive=io.BytesIO()
    with zipfile.ZipFile(archive,'w',compression=zipfile.ZIP_STORED) as output:
        output.writestr(str(paths[0]),b'ARCHIVE_ONE');output.writestr(str(paths[1]),b'ARCHIVE_TWO')
    data=archive.getvalue();corrupt=data.replace(b'ARCHIVE_TWO',b'CORRUPT_TWO',1)
    original=package.RangeZip;package.RangeZip=lambda url:io.BytesIO(corrupt)
    try:
        with contextlib.redirect_stdout(io.StringIO()):
            try:package.install(case,'cuda','1.2.3');raise AssertionError('CRC failure ignored')
            except zipfile.BadZipFile:pass
        old(target);assert not list(package.jobs_root(case).iterdir())
        package.RangeZip=lambda url:io.BytesIO(data)
        with contextlib.redirect_stdout(io.StringIO()):package.install(case,'cuda','1.2.3')
        assert target.joinpath(*paths[0].parts).read_bytes()==b'ARCHIVE_ONE'
        assert json.loads((target/'cuda-package.json').read_bytes())['version']=='1.2.3'
    finally:package.RangeZip=original
    print('PASS CRC failure leaves installation untouched; verified staged extraction publishes the matching marker')
finally:
    assert root.resolve().is_relative_to((project/'build-tools').resolve());shutil.rmtree(root)
