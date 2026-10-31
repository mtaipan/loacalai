"""Fetch matching official Ollama GPU libraries, without bundling weights.

HTTP byte ranges avoid downloading the entire multi-gigabyte release archive.
Every ZIP member is CRC checked by zipfile; extracted paths are allowlisted.
"""
import argparse,io,json,os,re,shutil,sys,time,urllib.request,zipfile
from pathlib import Path,PurePosixPath

class RangeZip(io.RawIOBase):
    block_size=4*1024*1024
    def __init__(self,url):
        super().__init__();self.opener=urllib.request.build_opener(urllib.request.ProxyHandler({}));self.url=url;self.pos=0;self.cache_start=-1;self.cache=b''
        with self._open(0,0) as response:
            match=re.fullmatch(r'bytes 0-0/(\d+)',response.headers.get('Content-Range',''))
            if response.status!=206 or not match:raise RuntimeError('Server must support HTTP byte ranges')
            self.size=int(match[1]);self.url=response.geturl();response.read()
    def _open(self,start,end):
        last=None
        for attempt in range(3):
            try:return self.opener.open(urllib.request.Request(self.url,headers={'Range':f'bytes={start}-{end}','User-Agent':'LocalGemmaStudio'}),timeout=60)
            except Exception as e:last=e;time.sleep(attempt)
        raise last
    def seekable(self):return True
    def readable(self):return True
    def tell(self):return self.pos
    def seek(self,offset,whence=0):
        self.pos=offset if whence==0 else self.pos+offset if whence==1 else self.size+offset
        if self.pos<0:raise ValueError('Negative ZIP position')
        return self.pos
    def read(self,n=-1):
        if n<0:n=self.size-self.pos
        n=min(n,self.size-self.pos)
        result=bytearray()
        while n>0:
            if not self.cache_start<=self.pos<self.cache_start+len(self.cache):
                start=self.pos//self.block_size*self.block_size;end=min(self.size-1,start+self.block_size-1)
                with self._open(start,end) as response:
                    if response.status!=206 or response.headers.get('Content-Range')!=f'bytes {start}-{end}/{self.size}':raise RuntimeError('Invalid HTTP byte range response')
                    self.cache=response.read(end-start+2)
                if len(self.cache)!=end-start+1:raise RuntimeError('Incomplete HTTP byte range')
                self.cache_start=start
            offset=self.pos-self.cache_start;take=min(n,len(self.cache)-offset)
            result.extend(self.cache[offset:offset+take]);self.pos+=take;n-=take
        return bytes(result)

def members(z,backend):
    for entry in z.infolist():
        path=PurePosixPath(entry.filename.replace('\\','/'))
        if path.is_absolute() or '..' in path.parts:continue
        parts=path.parts
        if len(parts)>=4 and parts[:2]==('lib','ollama'):
            folder=parts[2].lower()
            if (backend=='vulkan' and folder.startswith('vulkan')) or (backend=='cuda' and folder.startswith('cuda')) or (backend=='rocm' and folder.startswith('rocm')):
                if not entry.is_dir():yield entry,path

def install(root,backend,version,list_only=False):
    if not re.fullmatch(r'\d+\.\d+\.\d+(?:[-.][A-Za-z0-9]+)*',version):raise ValueError('Invalid Ollama version')
    if backend not in ['cuda','vulkan','rocm']:raise ValueError('Unknown backend')
    binary=root/'runtime/ollama/ollama.exe'
    with binary.open('rb') as file:
        file.seek(0x3c);offset=int.from_bytes(file.read(4),'little');file.seek(offset+4);machine=int.from_bytes(file.read(2),'little')
    arch={0x8664:'amd64',0xaa64:'arm64'}.get(machine)
    if not arch:raise RuntimeError('Unsupported engine binary architecture')
    asset='ollama-windows-'+arch+('-rocm' if backend=='rocm' else '')+'.zip'
    url=f'https://github.com/ollama/ollama/releases/download/v{version}/{asset}'
    print(json.dumps({'status':'index','backend':backend,'version':version}),flush=True)
    with RangeZip(url) as remote,zipfile.ZipFile(remote) as archive:
        selected=list(members(archive,backend))
        if not selected:raise RuntimeError('Matching GPU libraries are absent from the release archive')
        total=sum(e.file_size for e,_ in selected)
        print(json.dumps({'status':'files','bytes':total,'files':[str(p) for _,p in selected]}),flush=True)
        if list_only:return
        if shutil.disk_usage(root).free<total*2+256*1024*1024:raise RuntimeError('Not enough free space on the application drive')
        target=root/'runtime/ollama';stage=root/'data/temp'/('gpu-'+backend+'-'+str(os.getpid()));stage.mkdir(parents=True,exist_ok=True)
        try:
            done=0
            for entry,path in selected:
                destination=stage.joinpath(*path.parts);destination.parent.mkdir(parents=True,exist_ok=True)
                with archive.open(entry) as source,destination.open('wb') as output:shutil.copyfileobj(source,output,1024*1024)
                if destination.stat().st_size!=entry.file_size:raise RuntimeError('Incomplete library extraction')
                done+=entry.file_size;print(json.dumps({'status':'extract','file':str(path),'completed':done,'total':total}),flush=True)
            for _,path in selected:
                destination=target.joinpath(*path.parts);destination.parent.mkdir(parents=True,exist_ok=True)
                os.replace(stage.joinpath(*path.parts),destination)
            marker=target/f'{backend}-package.json';marker.write_text(json.dumps({'backend':backend,'version':version,'files':[str(p) for _,p in selected]}),encoding='utf-8')
            print(json.dumps({'status':'ready','backend':backend,'bytes':total}),flush=True)
        finally:shutil.rmtree(stage,ignore_errors=True)

if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('--root',type=Path,required=True);p.add_argument('--backend',choices=['cuda','vulkan','rocm'],required=True);p.add_argument('--version',required=True);p.add_argument('--list',action='store_true');a=p.parse_args()
    install(a.root.resolve(),a.backend,a.version,a.list)
