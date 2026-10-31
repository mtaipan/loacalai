"""Isolated hardware enumeration: a broken graphics driver cannot crash the UI."""
import argparse,ctypes as C,ctypes.util,json,os,platform,shutil,struct,subprocess,sys
from pathlib import Path

def run(args,seconds=12):
    return subprocess.check_output(args,timeout=seconds,creationflags=0x08000000 if os.name=='nt' else 0).decode('utf-8',errors='replace').strip()
def powershell(script,seconds=20,env=None):
    return subprocess.check_output(['powershell.exe','-NoProfile','-NonInteractive','-Command',"[Console]::OutputEncoding=[Text.UTF8Encoding]::new($false); "+script],timeout=seconds,env=env,creationflags=0x08000000).decode('utf-8',errors='replace').strip()
def vulkan():
    loader='vulkan-1.dll' if os.name=='nt' else ctypes.util.find_library('vulkan')
    if not loader:return []
    lib=C.WinDLL(loader) if os.name=='nt' else C.CDLL(loader)
    class App(C.Structure):_fields_=[('sType',C.c_uint32),('pNext',C.c_void_p),('name',C.c_char_p),('appVersion',C.c_uint32),('engine',C.c_char_p),('engineVersion',C.c_uint32),('apiVersion',C.c_uint32)]
    class Info(C.Structure):_fields_=[('sType',C.c_uint32),('pNext',C.c_void_p),('flags',C.c_uint32),('app',C.POINTER(App)),('layerCount',C.c_uint32),('layers',C.c_void_p),('extCount',C.c_uint32),('extensions',C.c_void_p)]
    lib.vkCreateInstance.argtypes=[C.POINTER(Info),C.c_void_p,C.POINTER(C.c_void_p)];lib.vkCreateInstance.restype=C.c_int32
    lib.vkEnumeratePhysicalDevices.argtypes=[C.c_void_p,C.POINTER(C.c_uint32),C.POINTER(C.c_void_p)];lib.vkEnumeratePhysicalDevices.restype=C.c_int32
    lib.vkGetPhysicalDeviceProperties.argtypes=[C.c_void_p,C.c_void_p];lib.vkGetPhysicalDeviceMemoryProperties.argtypes=[C.c_void_p,C.c_void_p];lib.vkDestroyInstance.argtypes=[C.c_void_p,C.c_void_p]
    app=App(0,None,b'LocalGemmaStudio',0,None,0,1<<22);info=Info(1,None,0,C.pointer(app),0,None,0,None);instance=C.c_void_p()
    code=lib.vkCreateInstance(C.byref(info),None,C.byref(instance))
    if code!=0:raise RuntimeError('Vulkan instance error '+str(code))
    try:
        count=C.c_uint32();code=lib.vkEnumeratePhysicalDevices(instance,C.byref(count),None)
        if code!=0 or count.value>64:raise RuntimeError('Vulkan enumeration failed')
        devices=(C.c_void_p*count.value)();code=lib.vkEnumeratePhysicalDevices(instance,C.byref(count),devices)
        if code not in (0,5):raise RuntimeError('Vulkan device enumeration failed')
        result=[]
        for index,device in enumerate(devices):
            props=(C.c_uint64*512)();mem=(C.c_uint64*512)();lib.vkGetPhysicalDeviceProperties(device,C.byref(props));lib.vkGetPhysicalDeviceMemoryProperties(device,C.byref(mem));data=bytes(props);memory=bytes(mem)
            api,driver,vendor,device_id,kind=struct.unpack_from('<IIIII',data);name=data[20:276].split(b'\0',1)[0].decode('utf-8',errors='replace')
            heaps=struct.unpack_from('<I',memory,260)[0];vram=sum(struct.unpack_from('<Q',memory,264+i*16)[0] for i in range(min(heaps,16)) if struct.unpack_from('<I',memory,272+i*16)[0]&1)
            result.append({'backend':'vulkan','device':str(index),'name':name,'vendor':{0x10de:'NVIDIA',0x1002:'AMD',0x8086:'Intel',0x106b:'Apple'}.get(vendor,hex(vendor)),'memory_bytes':vram,'api_version':f'{api>>22}.{api>>12&1023}.{api&4095}','device_type':kind})
        return result
    finally:lib.vkDestroyInstance(instance,None)

def inventory():
    result={'platform':sys.platform,'architecture':platform.machine(),'logical_cpus':os.cpu_count() or 1,'cpu_name':platform.processor() or platform.machine(),'devices':[],'display_adapters':[],'errors':[]}
    if os.name=='nt':
        try:
            data=json.loads(powershell("$taskCpu=Get-CimInstance Win32_Processor; $taskGpu=@(Get-CimInstance Win32_VideoController | Select-Object Name,AdapterCompatibility,DriverVersion); @{cpu=($taskCpu.Name -join ', ');cores=($taskCpu | Measure-Object NumberOfCores -Sum).Sum;adapters=$taskGpu} | ConvertTo-Json -Depth 4 -Compress"))
            result['cpu_name']=data['cpu'];result['physical_cores']=data['cores'];result['display_adapters']=data['adapters']
        except Exception as e:result['errors'].append('CPU/display enumeration: '+str(e))
    elif sys.platform=='darwin':
        try:
            data=json.loads(run(['system_profiler','SPDisplaysDataType','-json'],20));adapters=data.get('SPDisplaysDataType',[])
            result['display_adapters']=adapters
            silicon=platform.machine().lower() in ('arm64','aarch64') or any('Apple' in a.get('sppci_model','') for a in adapters)
            for adapter in (adapters if silicon else []):result['devices'].append({'backend':'metal','device':'','name':adapter.get('sppci_model',adapter.get('_name','Apple GPU')),'vendor':'Apple/Metal'})
            result['cpu_name']=run(['sysctl','-n','machdep.cpu.brand_string'])
        except Exception as e:result['errors'].append('Metal enumeration: '+str(e))
    else:
        try:
            for line in Path('/proc/cpuinfo').read_text().splitlines():
                if line.startswith('model name'):result['cpu_name']=line.split(':',1)[1].strip();break
        except OSError:pass
    if sys.platform!='darwin':
        try:result['devices']+=vulkan()
        except Exception as e:result['errors'].append('Vulkan enumeration: '+str(e))
    smi=shutil.which('nvidia-smi')
    if not smi and os.name=='nt' and Path('C:/Windows/System32/nvidia-smi.exe').is_file():smi='C:/Windows/System32/nvidia-smi.exe'
    if smi:
        try:
            import csv
            text=run([smi,'--query-gpu=uuid,name,memory.total','--format=csv,noheader,nounits'])
            for row in csv.reader(text.splitlines()):
                if len(row)==3:result['devices'].append({'backend':'cuda','device':row[0].strip(),'name':row[1].strip(),'vendor':'NVIDIA','memory_bytes':int(float(row[2].strip()))*1024*1024})
        except Exception as e:result['errors'].append('CUDA enumeration: '+str(e))
    return result

def stop_engine(executable,port):
    if port!=11435:raise ValueError('Only the private application port may be adopted')
    if os.name!='nt':raise RuntimeError('Legacy engine adoption is only available on Windows; restart the app-owned engine manually once')
    env={**os.environ,'LOCAL_GEMMA_EXPECTED_ENGINE':str(executable.resolve())}
    script=r"""$ErrorActionPreference='Stop'; $taskIds=@(Get-NetTCPConnection -LocalPort 11435 -State Listen -ErrorAction SilentlyContinue | Select-Object -ExpandProperty OwningProcess -Unique); if($taskIds.Count -ne 1){throw 'Private engine listener not found'}; $taskProcess=Get-CimInstance Win32_Process -Filter ('ProcessId='+$taskIds[0]); if(![string]::Equals($taskProcess.ExecutablePath,$env:LOCAL_GEMMA_EXPECTED_ENGINE,[StringComparison]::OrdinalIgnoreCase) -or $taskProcess.CommandLine -notmatch '\bserve\b'){throw 'Listener belongs to a different application; it was not stopped'}; & taskkill.exe /PID $taskIds[0] /T /F | Out-Null; if($LASTEXITCODE -ne 0){throw 'Engine could not be stopped'}; @{stopped=$true;pid=$taskIds[0]} | ConvertTo-Json -Compress"""
    return json.loads(powershell(script,25,env))

if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('--stop-engine',type=Path);p.add_argument('--port',type=int,default=11435);a=p.parse_args()
    print(json.dumps(stop_engine(a.stop_engine,a.port) if a.stop_engine else inventory(),ensure_ascii=False),flush=True)
