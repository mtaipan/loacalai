param([Parameter(Mandatory=$true)][string]$TaskRoot)
$ErrorActionPreference='Stop'
$taskCandidates=@("${env:ProgramFiles(x86)}\Microsoft\EdgeWebView\Application","$env:LOCALAPPDATA\Microsoft\EdgeWebView\Application")
foreach($taskCandidate in $taskCandidates){if(Test-Path -LiteralPath $taskCandidate){if(Get-ChildItem -LiteralPath $taskCandidate -Filter msedgewebview2.exe -Recurse -ErrorAction SilentlyContinue){Write-Output 'WebView2 is installed.';exit 0}}}
$taskInstaller=Join-Path $TaskRoot 'runtime\MicrosoftEdgeWebView2RuntimeInstallerX64.exe'
if(!(Test-Path -LiteralPath $taskInstaller)){throw 'Offline WebView2 installer is missing.'}
$taskProcess=Start-Process -FilePath $taskInstaller -ArgumentList '/silent','/install' -WindowStyle Hidden -PassThru -Wait
if($taskProcess.ExitCode -ne 0){throw "WebView2 installation failed: $($taskProcess.ExitCode)"}
Write-Output 'WebView2 installed from bundled offline component.'
