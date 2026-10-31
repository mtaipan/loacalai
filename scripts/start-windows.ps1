param([switch]$Browser,[switch]$SetupOnly,[switch]$Rebuild)
$ErrorActionPreference='Stop'
$taskRoot=[IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$env:LOCAL_GEMMA_HOME=$taskRoot
if($SetupOnly -or !(Test-Path -LiteralPath "$taskRoot\runtime\ollama\ollama.exe") -or !(Test-Path -LiteralPath "$taskRoot\runtime\python\python.exe")){
 & "$PSScriptRoot\provision-windows.ps1" -TaskRoot $taskRoot
 if(!$?){throw 'Setup failed'}
}
if($Rebuild){cargo build --release --locked --manifest-path "$taskRoot\Cargo.toml";if($LASTEXITCODE -ne 0){throw 'Build failed'};Copy-Item "$taskRoot\target\release\local-gemma-studio.exe" "$taskRoot\LocalGemmaStudio.exe" -Force}
if(!$SetupOnly){if($Browser){Start-Process -FilePath "$taskRoot\LocalGemmaStudio.exe" -ArgumentList '--browser' -WorkingDirectory $taskRoot -WindowStyle Hidden}else{Start-Process -FilePath "$taskRoot\LocalGemmaStudio.exe" -WorkingDirectory $taskRoot -WindowStyle Hidden}}
