'use strict';
const RUN_TOOL_WINDOW=48;
function trimRunView(session){
  if(session.streamTools.length>RUN_TOOL_WINDOW){
    const root=document.getElementById('stream-tools'),selection=getSelection();
    const active=root?.contains(document.activeElement)?document.activeElement.closest('[data-tool-key]')?.dataset.toolKey:null;
    const selected=selection?.anchorNode?.parentElement?.closest('#stream-tools [data-tool-key]')?.dataset.toolKey;
    const keep=new Set([active,selected].filter(Boolean));
    const recent=session.streamTools.slice(-RUN_TOOL_WINDOW),ids=new Set(recent.map(tool=>tool.id));
    session.streamTools=[...session.streamTools.slice(0,-RUN_TOOL_WINDOW).filter(tool=>keep.has(tool.id)&&!ids.has(tool.id)),...recent];
  }
  for(const[field,offset,limit]of[['streamThinking','thinkingOffset',16384],['streamText','textOffset',65536]]){
    const text=session[field]||'';if(text.length<=limit)continue;
    let cut=text.length-limit;if(text.charCodeAt(cut)>=0xDC00&&text.charCodeAt(cut)<=0xDFFF)cut++;
    session[offset]=(session[offset]||0)+cut;session[field]=text.slice(cut);
  }
}
function runHistoryLinks(run,session=null,diagnostics=null){
  if(!/^[0-9a-f-]{36}$/i.test(run||''))return '';
  const partial=session&&((session.toolsTotal||0)>session.streamTools.length||session.textOffset||session.thinkingOffset);
  const tools=session?(session.toolsTotal||0)>0:(diagnostics?.execution?.actions||0)>0;
  const thinking=session?session.thinkingOffset>0:diagnostics?.thinking_partial;
  const content=session?.textOffset>0;
  if(!tools&&!thinking&&!content)return '';
  return `<div class="run-history-links">${partial?'<p class="form-help">Показана последняя часть. Предыдущие действия и текст доступны в журнале.</p>':''}<div class="project-actions">${tools?`<button class="button" data-tool-history="${run}">Действия запуска</button>`:''}${thinking?`<button class="button" data-run-text="${run}" data-kind="thinking">Все рассуждения</button>`:''}${content?`<button class="button" data-run-text="${run}" data-kind="content">Вывод запуска</button>`:''}</div></div>`;
}
async function showRunHistory(run,kind='tools',local=null){
  const dialog=document.createElement('dialog');dialog.className='workspace-dialog run-history-dialog';dialog.setAttribute('aria-labelledby','run-history-title');
  dialog.innerHTML=`<h2 id="run-history-title">${kind==='tools'?'Действия запуска':kind==='thinking'?'Рассуждения':'Вывод запуска'}</h2><div class="dialog-body"><p class="form-help" role="status"></p><div class="run-history-content"></div></div><div class="dialog-footer"><button class="button history-older">Раньше</button><button class="button history-newer">Позже</button><button class="button history-latest">Последние</button><button class="button history-close">Закрыть</button></div>`;
  document.body.append(dialog);const find=selector=>dialog.querySelector(selector);let before=null,next=null,stack=[],controller=null,generation=0;
  dialog.onclose=()=>{controller?.abort();dialog.remove();};find('.history-close').onclick=()=>dialog.close();
  async function render(){
    controller?.abort();controller=new AbortController();const own=++generation;find('[role="status"]').textContent='Чтение журнала…';
    try{
      let page;
      if(local){const end=before??local.length,start=Math.max(0,end-32);page={tools:local.slice(start,end).map(tool=>({tool})),next_before:start||null};}
      else page=await api('/api/runs/'+run+'/'+(kind==='tools'?'tools':'text?kind='+kind)+(before?(kind==='tools'?'?':'&')+'before='+before:''),{signal:controller.signal});
      if(!dialog.open||own!==generation)return;next=page.next_before;
      const content=find('.run-history-content');
      if(kind==='tools'){content.className='run-history-content tool-log';content.innerHTML=page.tools.map(row=>toolHtml(row.tool,row.tool.status==='running')).join('');enhanceMarkdown(content);}
      else{content.className='run-history-content';content.replaceChildren();for(const row of page.items){if(row.type==='replace_content'){const marker=document.createElement('p');marker.className='form-help';marker.textContent='Ответ заменён следующим текстом:';content.append(marker);}const pre=document.createElement('pre');pre.className='artifact-content';pre.textContent=row.text;content.append(pre);}}
      find('[role="status"]').textContent=(kind==='tools'?page.tools.length:page.items.length)?'Записи этой страницы идут по времени.':'В этом разделе пока нет записей.';
      find('.history-older').disabled=!next;find('.history-newer').disabled=!stack.length;
    }catch(error){if(error.name!=='AbortError'&&dialog.open)find('[role="status"]').textContent='Не удалось прочитать журнал: '+error.message;}
  }
  find('.history-older').onclick=()=>{if(next){stack.push(before);before=next;render();}};
  find('.history-newer').onclick=()=>{if(stack.length){before=stack.pop();render();}};
  find('.history-latest').onclick=()=>{before=null;stack=[];render();};dialog.showModal();await render();
}
