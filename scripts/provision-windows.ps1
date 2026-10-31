param([Parameter(Mandatory=$true)][string]$TaskRoot)
$ErrorActionPreference='Stop'
$TaskRoot=[IO.Path]::GetFullPath($TaskRoot)
$taskRuntime=Join-Path $TaskRoot 'runtime'
$taskTemp=Join-Path $TaskRoot 'data\temp'
New-Item -ItemType Directory -Force $taskRuntime,$taskTemp,(Join-Path $TaskRoot 'data\models') | Out-Null
$env:TEMP=$taskTemp; $env:TMP=$taskTemp
$env:HF_HOME=Join-Path $TaskRoot 'data\cache\huggingface'
$env:PIP_CACHE_DIR=Join-Path $TaskRoot 'data\cache\pip'
$env:PYTHONUTF8='1'
$taskArch=if($env:PROCESSOR_ARCHITECTURE -eq 'ARM64' -or $env:PROCESSOR_ARCHITEW6432 -eq 'ARM64'){'arm64'}else{'amd64'}
[Net.ServicePointManager]::SecurityProtocol=[Net.SecurityProtocolType]::Tls12
function Get-TaskFile([string]$url,[string]$path){Write-Output "Downloading: $url";$taskClient=New-Object Net.WebClient;try{$taskClient.DownloadFile($url,$path)}finally{$taskClient.Dispose()}}
$taskOllama=Join-Path $taskRuntime 'ollama\ollama.exe'
if(!(Test-Path -LiteralPath $taskOllama)){
 $taskZip=Join-Path $taskTemp 'ollama.zip'
 Get-TaskFile "https://github.com/ollama/ollama/releases/latest/download/ollama-windows-$taskArch.zip" $taskZip
 Add-Type -AssemblyName System.IO.Compression.FileSystem
 $taskArchive=[IO.Compression.ZipFile]::OpenRead($taskZip)
 try{foreach($taskEntry in $taskArchive.Entries){
  $taskName=$taskEntry.FullName.Replace('\','/')
  if($taskName -eq 'ollama.exe' -or $taskName -match '^lib/ollama/[^/]+$'){
   $taskDest=Join-Path (Join-Path $taskRuntime 'ollama') $taskName
   New-Item -ItemType Directory -Force ([IO.Path]::GetDirectoryName($taskDest)) | Out-Null
   [IO.Compression.ZipFileExtensions]::ExtractToFile($taskEntry,$taskDest,$true)
  }
 }}finally{$taskArchive.Dispose()}
 Remove-Item -LiteralPath $taskZip
}
$taskPython=Join-Path $taskRuntime 'python\python.exe'
if(!(Test-Path -LiteralPath $taskPython)){
 $taskZip=Join-Path $taskTemp 'python.zip'
 Get-TaskFile "https://www.python.org/ftp/python/3.13.15/python-3.13.15-embed-$taskArch.zip" $taskZip
 Expand-Archive -LiteralPath $taskZip -DestinationPath (Join-Path $taskRuntime 'python') -Force
 Remove-Item -LiteralPath $taskZip
}
@('python313.zip','.','..','Lib\site-packages','import site') | Set-Content -Encoding ASCII (Join-Path $taskRuntime 'python\python313._pth')
& $taskPython -c 'import faster_whisper, sounddevice, PIL, pypdfium2, playwright'
if($LASTEXITCODE -ne 0){
 $taskPip=Join-Path $taskTemp 'get-pip.py'
 Get-TaskFile 'https://bootstrap.pypa.io/get-pip.py' $taskPip
 & $taskPython $taskPip --no-warn-script-location
 if($LASTEXITCODE -ne 0){throw 'pip bootstrap failed'}
 & $taskPython -m pip install --no-warn-script-location -r (Join-Path $taskRuntime 'requirements.txt')
 if($LASTEXITCODE -ne 0){throw 'Media dependency installation failed'}
}
if(!(Test-Path -LiteralPath (Join-Path $TaskRoot 'data\models\whisper-small\model.bin'))){
 & $taskPython (Join-Path $taskRuntime 'setup_media.py')
 if($LASTEXITCODE -ne 0){throw 'Whisper download failed'}
}
$taskShell=New-Object -ComObject WScript.Shell
$taskShortcut=$taskShell.CreateShortcut((Join-Path ([Environment]::GetFolderPath('Desktop')) 'Local Gemma Studio.lnk'))
$taskShortcut.TargetPath=Join-Path $TaskRoot 'LocalGemmaStudio.exe'
$taskShortcut.WorkingDirectory=$TaskRoot
$taskShortcut.Save()
Write-Output 'Ready. LLM weights can be downloaded or removed in Settings.'
