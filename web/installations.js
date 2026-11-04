"use strict";
const installationLabels={queued:'В очереди',running:'Установка',stopping:'Остановка…',succeeded:'Установлено',failed:'Ошибка',cancelled:'Остановлено',interrupted:'Прервано при закрытии приложения'};
const installationActive=job=>['queued','running','stopping'].includes(job.status);
function installationName(job){const spec=job.spec||{};return spec.kind==='models'?(spec.models||[]).map(modelName).join(', '):spec.kind==='gpu'?'Модуль '+String(spec.backend).toUpperCase():'Браузер';}
function installationProgress(event){const p=event.progress;if(p)return (event.model?modelName(event.model)+' · ':'')+(p.status||'')+(p.total?' · '+Math.min(100,Math.floor(100*(p.completed||0)/p.total))+'%':'');return event.message||'';}
async function runInstallation(spec,onProgress=()=>{}){
  const id=crypto.randomUUID();let job;
  try{job=await api('/api/installations',{method:'POST',body:{request_id:id,spec}});}
  catch(error){try{job=await api('/api/installations/'+id);}catch{throw error;}}
  let after=0;onProgress(installationLabels[job.status]||job.status);
  while(true){
    let page;try{page=await api('/api/installations/'+job.id+'/events?after='+after);}
    catch(error){throw new Error('Связь с приложением потеряна. Ход установки доступен в разделе «Установки». '+error.message);}
    job=page.job;for(const item of page.items){after=item.sequence;const text=installationProgress(item.event);if(text)onProgress(text);}
    if(!page.has_more&&!installationActive(job)){
      onProgress(installationLabels[job.status]+(job.error?' · '+job.error:''));
      if(job.status!=='succeeded')throw new Error(job.error||installationLabels[job.status]);return job;
    }
    if(!page.has_more)await new Promise(resolve=>setTimeout(resolve,600));
  }
}
async function showInstallations(){
  const existing=document.getElementById('installations-dialog');if(existing){existing.focus();return;}
  const opener=document.activeElement,dialog=document.createElement('dialog');dialog.id='installations-dialog';dialog.className='workspace-dialog';dialog.setAttribute('aria-labelledby','installations-title');
  dialog.innerHTML='<div class="dialog-head"><h2 id="installations-title">Установки</h2><button class="icon-button installations-close" aria-label="Закрыть">×</button></div><div class="dialog-body"><p class="form-help">Закрытие этого окна не останавливает установку. После перезапуска приложения прерванную установку можно повторить; доступные сохранённые части будут проверены.</p><p role="status" class="installation-state"></p><div class="installation-list"></div><button class="button installations-newer" hidden>К новым установкам</button><button class="button installations-older" hidden>Предыдущие установки</button></div>';
  document.body.append(dialog);let timer=null,version=0,pending=false,cursor=null,currentCursor=null,cursorHistory=[],items=new Map(),progress=new Map(),eventCursors=new Map(),eventPending=new Set();const find=s=>dialog.querySelector(s);
  dialog.onclose=()=>{version++;clearTimeout(timer);dialog.remove();if(opener?.isConnected)opener.focus({preventScroll:true});};find('.installations-close').onclick=()=>dialog.close();
  function render(){
    const list=find('.installation-list');const jobs=[...items.values()].sort((a,b)=>b.created.localeCompare(a.created)||b.id.localeCompare(a.id));
    if(!jobs.length){list.textContent='Установок пока нет.';return;}
    if(!list.querySelector('article'))list.textContent='';
    for(const node of [...list.children])if(!items.has(node.dataset.job)){if(node.contains(document.activeElement))find('.installation-state').focus({preventScroll:true});node.remove();}
    jobs.forEach((job,index)=>{
      let node=[...list.children].find(n=>n.dataset.job===job.id);
      if(!node){node=document.createElement('article');node.className='installation-item';node.dataset.job=job.id;node.innerHTML='<h3></h3><p class="installation-status" tabindex="-1"></p><p class="form-help installation-progress"></p><div class="installation-controls"></div><details><summary>Сведения</summary><p class="form-help installation-description"></p><button class="button" data-installation-action="journal">Журнал установки</button></details>';node.querySelector('[data-installation-action]').dataset.installationId=job.id;node.querySelector('h3').textContent=installationName(job);node.querySelector('.installation-description').textContent=job.created+(job.spec.directory?' · '+job.spec.directory:'');}
      if(list.children[index]!==node)list.insertBefore(node,list.children[index]||null);
      const status=node.querySelector('.installation-status'),progressNode=node.querySelector('.installation-progress'),text=(installationLabels[job.status]||job.status)+' · попытка '+Number(job.attempt),detail=job.error||progress.get(job.id)||'';
      if(status.textContent!==text)status.textContent=text;if(progressNode.textContent!==detail)progressNode.textContent=detail;
      if(node.dataset.status!==job.status){const controls=node.querySelector('.installation-controls'),focused=controls.contains(document.activeElement);controls.replaceChildren();
        const action=installationActive(job)?'cancel':['failed','cancelled','interrupted'].includes(job.status)?'retry':null;
        if(action){const button=document.createElement('button');button.className='button';button.dataset.installationId=job.id;button.dataset.installationAction=action;button.textContent=action==='cancel'?'Остановить':'Повторить';button.disabled=job.status==='stopping';controls.append(button);if(focused&&!button.disabled)button.focus({preventScroll:true});else if(focused)status.focus({preventScroll:true});}else if(focused)status.focus({preventScroll:true});node.dataset.status=job.status;
      }
    });
  }
  async function refresh(){
    if(pending||!dialog.open)return;pending=true;find('.installations-older').disabled=true;find('.installations-newer').disabled=true;const request=version;
    try{
      const page=await api('/api/installations'+(currentCursor?'?'+new URLSearchParams(currentCursor):''));if(!dialog.open||request!==version)return;
      cursor=page.next;const listed=new Set(page.items.map(j=>j.id));for(const id of items.keys())if(!listed.has(id)){items.delete(id);progress.delete(id);eventCursors.delete(id);eventPending.delete(id);}
      for(const job of page.items)items.set(job.id,job);
      // Refresh the selected older jobs as well as the newest page without loading their journals wholesale.
      for(const [id,old]of items){if(!page.items.some(j=>j.id===id)&&installationActive(old))items.set(id,await api('/api/installations/'+id));}
      for(const [id,job]of items){if(installationActive(job)||eventPending.has(id)||!eventCursors.has(id)){const events=await api('/api/installations/'+id+'/events?after='+(eventCursors.get(id)||0));if(!dialog.open||request!==version)return;items.set(id,events.job);eventCursors.set(id,events.next_after);for(const row of events.items){const text=installationProgress(row.event);if(text)progress.set(id,text);}if(events.has_more)eventPending.add(id);else eventPending.delete(id);}}
      find('.installation-state').textContent='';find('.installations-older').hidden=!cursor;find('.installations-newer').hidden=!cursorHistory.length;render();
    }catch(error){if(dialog.open)find('.installation-state').textContent='Не удалось обновить установки: '+error.message;}
    finally{pending=false;clearTimeout(timer);if(dialog.open){find('.installations-older').disabled=false;find('.installations-newer').disabled=false;timer=setTimeout(()=>refresh(),1200);}}
  }
  find('.installation-state').tabIndex=-1;find('.installations-older').onclick=()=>{if(pending||!cursor)return;cursorHistory.push(currentCursor);currentCursor=cursor;items.clear();eventCursors.clear();eventPending.clear();progress.clear();refresh();};find('.installations-newer').onclick=()=>{if(pending||!cursorHistory.length)return;currentCursor=cursorHistory.pop();items.clear();eventCursors.clear();eventPending.clear();progress.clear();refresh();};
  dialog.onclick=async event=>{const button=event.target.closest('[data-installation-action]');if(!button)return;const id=button.dataset.installationId,action=button.dataset.installationAction;button.disabled=true;
    try{if(action==='journal'){await showInstallationJournal(id);return;}const job=await api('/api/installations/'+id+'/'+action,{method:'POST',body:{}});items.set(id,job);eventCursors.delete(id);eventPending.delete(id);progress.delete(id);render();await refresh();}
    catch(error){find('.installation-state').textContent=error.message;}finally{if(button.isConnected)button.disabled=false;}
  };
  dialog.showModal();await refresh();
}
async function showInstallationJournal(id){
  const opener=document.activeElement,dialog=document.createElement('dialog');dialog.className='workspace-dialog';dialog.setAttribute('aria-label','Журнал установки');dialog.innerHTML='<div class="dialog-head"><h2>Журнал установки</h2><button class="button journal-close">Закрыть</button></div><div class="dialog-body"><pre class="artifact-content" tabindex="0"></pre><p role="status"></p><button class="button journal-next">Следующая часть</button></div>';document.body.append(dialog);let after=0,version=0;
  const find=s=>dialog.querySelector(s);dialog.onclose=()=>{version++;dialog.remove();if(opener?.isConnected)opener.focus({preventScroll:true});};find('.journal-close').onclick=()=>dialog.close();
  async function next(){const request=version;find('.journal-next').disabled=true;try{const page=await api('/api/installations/'+id+'/events?after='+after);if(!dialog.open||request!==version)return;find('pre').textContent=page.items.map(row=>row.sequence+' · '+installationProgress(row.event)).join('\n')||'Новых записей нет.';after=page.next_after;find('[role=status]').textContent=installationLabels[page.job.status];find('.journal-next').disabled=!page.has_more&&!installationActive(page.job);find('pre').focus({preventScroll:true});}catch(error){if(dialog.open){find('[role=status]').textContent=error.message;find('.journal-next').disabled=false;}}}
  find('.journal-next').onclick=next;dialog.showModal();await next();
}
