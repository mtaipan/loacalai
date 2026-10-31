param([Parameter(Mandatory=$true)][string]$TaskRoot)
$ErrorActionPreference='Stop'
$taskBase=[IO.Path]::GetFullPath($TaskRoot).TrimEnd('\')
$taskManifest=Join-Path $taskBase 'install-manifest.txt'
if(!(Test-Path -LiteralPath $taskManifest)){throw 'Install manifest is missing; no files were removed.'}
$taskPaths=Get-Content -LiteralPath $taskManifest
# Remove only exact files shipped in this installation. User data and models stay.
foreach($taskRelative in $taskPaths){
 $taskPath=[IO.Path]::GetFullPath((Join-Path $taskBase $taskRelative))
 if(!$taskPath.StartsWith($taskBase+'\',[StringComparison]::OrdinalIgnoreCase)){throw 'Invalid install manifest path'}
 if(Test-Path -LiteralPath $taskPath -PathType Leaf){Remove-Item -LiteralPath $taskPath -Force}
}
$taskShortcut=Join-Path ([Environment]::GetFolderPath('Desktop')) 'Local Gemma Studio.lnk'
if(Test-Path -LiteralPath $taskShortcut){$taskShell=New-Object -ComObject WScript.Shell;if($taskShell.CreateShortcut($taskShortcut).TargetPath -eq (Join-Path $taskBase 'LocalGemmaStudio.exe')){Remove-Item -LiteralPath $taskShortcut}}
Remove-Item -LiteralPath $taskManifest
Remove-Item -LiteralPath 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\LocalGemmaStudio' -ErrorAction SilentlyContinue
Write-Output "Application files removed. Chats, settings, downloaded models and optional GPU packages were preserved: $taskBase"
