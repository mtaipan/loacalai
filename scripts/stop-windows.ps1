$ErrorActionPreference='Stop'
$taskPort=if($env:LOCAL_GEMMA_PORT){$env:LOCAL_GEMMA_PORT}else{'3210'}
$taskUrl="http://127.0.0.1:$taskPort"
try {
  $taskApp=Invoke-RestMethod "$taskUrl/api/bootstrap" -TimeoutSec 2
  if($taskApp.app -ne 'Local Gemma Studio'){throw 'Unexpected server'}
  Invoke-RestMethod "$taskUrl/api/shutdown" -Method Post -Headers @{'X-Studio-Token'=$taskApp.token} -ContentType 'application/json' -Body '{}' | Out-Null
  Write-Host 'Local Gemma Studio server stopped. Ollama unloads idle models automatically.'
} catch {Write-Host 'Local Gemma Studio is not running or could not be stopped.'}
