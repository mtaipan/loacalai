'use strict';
function sourceIndexLink(message){const run=message.diagnostics?.source_index?.run||(message.diagnostics?.search?message.diagnostics?.execution?.run_id:null);return /^[0-9a-f-]{36}$/i.test(run||'')?`<button class="button" data-source-index="${run}">Веб-источники задачи</button>`:'';}
function citationReportHtml(message){
 const value=message.diagnostics?.citations;if(!value)return '';
 const notes=[];if(value.status==='unavailable')notes.push('Не удалось проверить происхождение ссылок.');
 if(value.missing_ids)notes.push('Не найдены источники для ссылок: '+value.missing_ids+'.');
 if(value.unverified_links)notes.push('Нет сохранённых источников для адресов: '+value.unverified_links+'.');
 if(value.not_received)notes.push('Для ссылок сохранён только адрес или выдача: '+value.not_received+'.');
 if(value.indexing?.has_more)notes.push('Журнал прежних источников ещё индексируется.');
 if(value.indexing?.errors?.count)notes.push('Не удалось восстановить источников: '+value.indexing.errors.count+'.');
 if(!notes.length)return '';
 return `<div class="citation-report"><p class="form-help">${notes.map(escapeHtml).join(' ')}</p>${value.artifact?.id?`<button class="button" data-artifact="${escapeHtml(value.artifact.id)}">Происхождение ссылок</button>`:''}</div>`;
}
async function showSourceIndex(run){
 const dialog=document.createElement('dialog');dialog.className='workspace-dialog source-index';dialog.setAttribute('aria-label','Веб-источники задачи');
 dialog.innerHTML='<h2>Веб-источники задачи</h2><div class="dialog-body"><p class="form-help">Статус относится к получению страницы. Смысловое подтверждение утверждений не проверено.</p><div class="source-index-list"></div><div class="source-index-errors"></div><p role="status"></p></div><div class="dialog-footer"><button class="button source-previous">Назад</button><button class="button source-next">Дальше</button><button class="button source-refresh">Обновить</button><button class="button source-retry" hidden>Повторить ошибки</button><button class="button source-close">Закрыть</button></div>';
 document.body.append(dialog);const find=selector=>dialog.querySelector(selector);let controller=null,version=0,before=null,snapshot=null,page=null,history=[];
 dialog.onclose=()=>{controller?.abort();dialog.remove();};find('.source-close').onclick=()=>dialog.close();
 async function render(retry=false){
  controller?.abort();controller=new AbortController();const current=++version;for(const selector of ['.source-next','.source-previous','.source-refresh','.source-retry'])find(selector).disabled=true;
  find('[role="status"]').textContent='Чтение источников…';
  try{const params=new URLSearchParams();if(before!=null)params.set('before',before);if(snapshot!=null)params.set('snapshot',snapshot);if(retry)params.set('retry_errors','true');const result=await api('/api/runs/'+run+'/sources?'+params,{signal:controller.signal});if(!dialog.open||current!==version)return;page=result;snapshot=result.snapshot;const list=find('.source-index-list');list.replaceChildren();
   for(const source of result.items){let url;try{url=new URL(source.url);if(!['http:','https:'].includes(url.protocol))continue;}catch{continue;}
    const row=document.createElement('article');row.className='source-index-row';const link=document.createElement('a');link.href=url.href;link.target='_blank';link.rel='noopener noreferrer';link.textContent=source.title||url.hostname;row.append(link);
    const state=document.createElement('p');state.className='form-help';state.textContent=url.hostname+' · '+({found:'Адрес найден поиском',linked:'Ссылка со страницы',received:'Ответ страницы сохранён',partial:'Сохранена часть ответа',unavailable:'Страница недоступна',redirect:'Перенаправление'}[source.state]||source.state)+(source.retrieved?' · '+new Date(source.retrieved).toLocaleString('ru'):'');row.append(state);
    if(source.published){const date=document.createElement('p');date.className='form-help';date.textContent='Дата от провайдера: '+source.published;row.append(date);}
    if(source.latest_state==='unavailable'&&source.state!=='unavailable'){const latest=document.createElement('p');latest.className='form-help';latest.textContent='При более позднем обращении страница была недоступна.';row.append(latest);}
    const receipt=document.createElement('button');receipt.className='button';receipt.textContent='Сохранённый результат';receipt.onclick=()=>showArtifact(source.artifact);row.append(receipt);list.append(row);
   }
   const errors=find('.source-index-errors');errors.replaceChildren();for(const error of result.indexing?.errors?.items||[]){const row=document.createElement('p');row.className='form-help';row.textContent='Источник не восстановлен: '+error.error;errors.append(row);}find('.source-retry').hidden=!result.indexing?.errors?.count;find('.source-retry').disabled=false;
   find('[role="status"]').textContent=(result.items.length?'Источников на странице: '+result.items.length:'Источников в индексе нет.')+(result.indexing?.has_more?' Предыдущие результаты индексируются. Нажмите «Обновить» для следующего пакета.':'');find('.source-next').disabled=!result.has_more;find('.source-previous').disabled=!history.length;find('.source-refresh').disabled=false;
  }catch(error){if(error.name!=='AbortError'&&dialog.open){find('[role="status"]').textContent=error.message;find('.source-refresh').disabled=false;}}
 }
 find('.source-next').onclick=()=>{history.push(before);before=page.next_before;render();};find('.source-previous').onclick=()=>{before=history.pop();render();};find('.source-refresh').onclick=()=>{before=null;snapshot=null;history=[];render();};find('.source-retry').onclick=()=>{before=null;snapshot=null;history=[];render(true);};dialog.showModal();await render();
}
