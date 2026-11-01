'use strict';
function processStateLabel(report){
  if(report.exit_code!=null)return 'Код завершения: '+report.exit_code;
  return {starting:'Запуск процесса',running:'Процесс работает',cancelled:'Процесс остановлен',timed_out:'Истёк заданный срок',interrupted:'Состояние процесса после перезапуска неизвестно',failed:'Ошибка процесса'}[report.status]||'Состояние не определено';
}
function renderProcessRecovery(){
  const errors=state.status?.process_recovery_errors||[];let element=document.getElementById('process-recovery');
  if(!errors.length){element?.remove();return;}
  if(!element){element=document.createElement('details');element.id='process-recovery';element.className='process-recovery';document.querySelector('.conversation-scroll').before(element);}
  element.replaceChildren();const summary=document.createElement('summary');summary.textContent='Не удалось восстановить часть журналов процессов';element.append(summary);
  const text=document.createElement('pre');text.textContent=errors.join('\n');element.append(text);
}
async function showProcess(id){
  document.getElementById('process-dialog')?.close();
  const dialog=document.createElement('dialog');dialog.id='process-dialog';dialog.className='workspace-dialog';dialog.setAttribute('aria-labelledby','process-title');
  dialog.innerHTML='<h2 id="process-title">Процесс</h2><div class="dialog-body"><p class="process-state" role="status"></p><pre class="process-command"></pre><div class="project-actions"><label>Поток <select class="process-stream"><option value="stdout">stdout</option><option value="stderr">stderr</option></select></label><label><input type="checkbox" class="process-follow" checked> Последние строки</label><button class="button process-refresh">Обновить</button></div><pre class="process-output" tabindex="0" aria-label="Вывод процесса"></pre><p class="process-position form-help"></p><div class="project-actions"><button class="button process-previous">Назад</button><button class="button process-next">Дальше</button><a class="button process-download" download>Скачать снимок потока</a></div><label class="form-label">Стандартный ввод<textarea class="full-input process-input" rows="3"></textarea></label><p class="form-help">Отправляется точный текст, без автоматического Enter. Программы, которым нужен настоящий терминал, могут не поддерживать такой ввод.</p><button class="button process-send">Отправить в процесс</button><p class="process-error form-help"></p></div><div class="dialog-footer"><button class="button process-stop">Остановить процесс</button><button class="button process-close">Закрыть окно</button></div>';
  document.body.append(dialog);const find=selector=>dialog.querySelector(selector);
  let timer=null,controller=null,version=0,offset=0,page=null;
  dialog.onclose=()=>{clearTimeout(timer);controller?.abort();dialog.remove();};find('.process-close').onclick=()=>dialog.close();
  async function refresh(){
    clearTimeout(timer);if(!dialog.open)return;controller?.abort();controller=new AbortController();const generation=++version,signal=controller.signal;
    try{
      const report=await api('/api/executions/'+id,{signal});if(generation!==version||!dialog.open)return;
      find('.process-state').textContent=processStateLabel(report);find('.process-command').textContent=(report.directory||'')+'\n'+(report.command||'');
      const running=['starting','running'].includes(report.status);find('.process-stop').disabled=!running;find('.process-send').disabled=!report.stdin_available;find('.process-input').disabled=!report.stdin_available;
      const stream=find('.process-stream').value,total=report[stream+'_page']?.bytes||0;
      if(find('.process-follow').checked)offset=Math.max(0,total-16384);
      const result=await api('/api/executions/'+id+'?stream='+stream+'&offset='+offset,{signal});if(generation!==version||!dialog.open)return;page=result;
      const output=find('.process-output');if(!selectionInside(output))output.textContent=page.text||'[Поток пуст]';
      find('.process-position').textContent='Байты '+page.offset+'–'+page.next_offset+' из '+page.bytes+(page.decoding_lossy?' · часть байтов не распознана как UTF-8; исходный поток сохранён':'');
      find('.process-download').href=page.raw_download;find('.process-previous').disabled=offset===0;find('.process-next').disabled=!page.has_more;
      find('.process-error').textContent=[report.error,report.note,report.log_sync_error,report.persistence_error].filter(Boolean).join('\n');
      if(running)timer=setTimeout(refresh,1000);
    }catch(error){if(error.name!=='AbortError'&&dialog.open)find('.process-error').textContent=error.message;}
  }
  find('.process-refresh').onclick=refresh;find('.process-stream').onchange=()=>{offset=0;refresh();};find('.process-follow').onchange=refresh;
  find('.process-previous').onclick=()=>{find('.process-follow').checked=false;offset=Math.max(0,offset-16384);refresh();};find('.process-next').onclick=()=>{find('.process-follow').checked=false;offset=page?.next_offset||offset;refresh();};
  find('.process-stop').onclick=async()=>{find('.process-stop').disabled=true;try{await api('/api/executions/'+id+'/stop',{method:'POST',body:{}});await refresh();}catch(error){find('.process-error').textContent=error.message;}};
  find('.process-send').onclick=async()=>{const input=find('.process-input'),text=input.value;find('.process-send').disabled=true;try{await api('/api/executions/'+id+'/input',{method:'POST',body:{text}});if(input.value===text)input.value='';await refresh();}catch(error){find('.process-error').textContent=error.message;find('.process-send').disabled=false;}};
  dialog.showModal();await refresh();
}
