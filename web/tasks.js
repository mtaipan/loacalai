'use strict';
function renderTaskIntent(){
  let element=document.getElementById('task-intent');
  const session=currentSession(),needed=session.nextTask||session.selectedTask;
  if(!needed){element?.remove();return;}
  if(!element){element=document.createElement('div');element.id='task-intent';element.className='task-intent';document.querySelector('.composer').before(element);}
  element.replaceChildren(document.createTextNode(session.nextTask?'Следующее сообщение: новая задача агента':'Следующее сообщение: продолжение выбранной задачи агентом'));
  const clear=document.createElement('button');clear.className='button';clear.textContent='Сбросить';clear.onclick=()=>{session.nextTask=false;session.selectedTask=null;captureDraft();renderTaskIntent();};element.append(clear);
}
async function showTaskState(){
  if(!state.chat){toast('Задача появится после отправки сообщения агенту.');return;}
  let dialog=document.getElementById('task-dialog');
  if(!dialog){dialog=document.createElement('dialog');dialog.id='task-dialog';dialog.className='workspace-dialog';dialog.setAttribute('aria-labelledby','task-dialog-title');document.body.append(dialog);}
  dialog.replaceChildren();
  const title=document.createElement('h2');title.id='task-dialog-title';title.textContent='Задачи в чате';dialog.append(title);
  const body=document.createElement('div');body.className='dialog-body';dialog.append(body);
  const footer=document.createElement('div');footer.className='dialog-footer';
  const fresh=document.createElement('button');fresh.className='button';fresh.textContent='Начать новую задачу';fresh.onclick=()=>{currentSession().nextTask=true;currentSession().selectedTask=null;captureDraft();renderTaskIntent();dialog.close();document.getElementById('prompt').focus();};
  const close=document.createElement('button');close.className='button';close.textContent='Закрыть';close.onclick=()=>dialog.close();footer.append(fresh,close);dialog.append(footer);dialog.showModal();
  const conversation=state.chat.id;
  try{
    const tasks=await api('/api/chats/'+conversation+'/tasks');if(!dialog.open)return;
    if(!tasks.length){body.textContent='В этом чате пока нет задач агента.';return;}
    const picker=document.createElement('select');picker.className='full-input';picker.setAttribute('aria-label','Сохранённая задача');
    tasks.forEach(task=>picker.add(new Option(task.goal_preview||'Задача с вложениями',task.id)));body.append(picker);
    const content=document.createElement('div');body.append(content);let generation=0;
    async function renderTask(){
      const version=++generation,task=await api('/api/tasks/'+picker.value);if(version!==generation||!dialog.open)return;
      content.replaceChildren();
      const project=document.createElement('p');project.className='form-help';project.textContent='Рабочая папка: '+(task.project||'Не указана');content.append(project);
      const resume=document.createElement('button');resume.className='button';resume.textContent='Выбрать для продолжения';resume.onclick=()=>{currentSession().selectedTask=task.id;currentSession().nextTask=false;captureDraft();renderTaskIntent();dialog.close();document.getElementById('prompt').focus();};content.append(resume);
      if(task.legacy_context){const legacy=document.createElement('p');legacy.className='form-help';legacy.textContent='Запросы перенесены из старого чата. Их принадлежность одной задаче и результаты прежних действий не проверены.';content.append(legacy);}
      const note=document.createElement('p');note.className='form-help';note.textContent='Отметки плана выставляет агент. Завершённый ответ не подтверждает готовность всей задачи.';content.append(note);
      appendTaskLedger(content,task,()=>version===generation&&dialog.open);
      for(const process of task.active_processes||[]){const button=document.createElement('button');button.className='button';button.textContent='Процесс: '+process.command_preview;button.onclick=()=>showProcess(process.execution_id);content.append(button);}
      if(task.material_jobs?.items?.length){const jobs=document.createElement('details'),heading=document.createElement('summary');heading.textContent='Подготовка материалов';jobs.append(heading);content.append(jobs);
        const append=page=>{for(const job of page.items){const row=document.createElement('p');row.className='form-help';row.textContent=(job.status==='ingesting'?'Подготовка':'Подготовка прервана')+' · '+job.path+' · прочитано '+job.next_byte+' байт · '+job.encoding+(job.error?' · '+job.error:'');jobs.append(row);}if(page.next_before){const more=document.createElement('button');more.className='button';more.textContent='Предыдущие операции';jobs.append(more);more.onclick=async()=>{more.disabled=true;try{const value=await api('/api/tasks/'+task.id+'?before_material='+page.next_before);more.remove();append(value.material_jobs);}catch(error){more.disabled=false;toast(error.message,true);}};}};append(task.material_jobs);
      }
      if(task.observations?.items?.length){const observations=document.createElement('details'),heading=document.createElement('summary');heading.textContent='Сохранённые результаты инструментов';observations.append(heading);content.append(observations);
        const append=page=>{for(const source of page.items){const button=document.createElement('button');button.className='button';button.textContent=toolTitle(source.metadata?.tool_name||source.kind)+(source.metadata?.status?' · '+toolStatusTitle(source.metadata.status):'');button.onclick=()=>showArtifact(source.id);observations.append(button);}if(page.has_more){const more=document.createElement('button');more.className='button';more.textContent='Предыдущие результаты';observations.append(more);more.onclick=async()=>{more.disabled=true;try{const earlier=await api('/api/tasks/'+task.id+'?before_artifact='+page.next_before);more.remove();append(earlier.observations);}catch(error){more.disabled=false;toast(error.message,true);}};}};append(task.observations);
      }
      if(task.plan){const list=document.createElement('ol');for(const step of task.plan.steps){const item=document.createElement('li');item.textContent=({pending:'Ожидает',in_progress:'В работе',completed:'Отмечено агентом как выполненное'}[step.status]||step.status)+': '+step.title;list.append(item);}content.append(list);}
      const requests=document.createElement('div');content.append(requests);
      function appendRequests(page){
        for(const request of page.requests){
          const section=document.createElement('section'),button=document.createElement('button');button.className='button';button.textContent='Запрос '+request.sequence+' · '+request.characters+' символов';
          const text=document.createElement('pre');text.style.cssText='white-space:pre-wrap;overflow-wrap:anywhere';text.textContent=request.preview;section.append(button,text);requests.append(section);
          button.onclick=async()=>{try{const part=await api('/api/tasks/'+task.id+'/requests/'+request.sequence);text.textContent=part.text;let next=part.next_offset;if(part.has_more){const more=document.createElement('button');more.className='button';more.textContent='Следующая часть';section.append(more);more.onclick=async()=>{more.disabled=true;try{const value=await api('/api/tasks/'+task.id+'/requests/'+request.sequence+'?offset='+next);text.textContent=value.text;next=value.next_offset;if(!value.has_more)more.remove();}catch(error){toast(error.message,true);}finally{more.disabled=false;}};}button.disabled=true;}catch(error){toast(error.message,true);}};
        }
        if(page.has_more){const more=document.createElement('button');more.className='button';more.textContent='Ещё запросы';requests.append(more);more.onclick=async()=>{more.disabled=true;try{const next=await api('/api/tasks/'+task.id+'?after='+page.next_after);more.remove();appendRequests(next);}catch(error){more.disabled=false;toast(error.message,true);}};}
      }
      appendRequests(task);
    }
    picker.onchange=()=>renderTask().catch(error=>toast(error.message,true));await renderTask();
  }catch(error){body.textContent='Не удалось загрузить задачу: '+error.message;}
}

function appendTaskLedger(parent,task,current){
  const node=(tag,text,cls)=>{const el=document.createElement(tag);if(text!==undefined)el.textContent=text;if(cls)el.className=cls;return el;};
  const labels={pending:'Не выполнено',in_progress:'В работе',reported_complete:'Агент считает выполненным',blocked:'Заблокировано',active:'Действует',waived:'Снято пользователем',superseded:'Заменено пользователем',passed:'Условие выполнено',failed:'Условие не выполнено',unknown:'Нет данных'};
  for(const [section,title] of [['requirements','Требования задачи'],['requirement_checks','Проверки по результатам действий'],['attempts','Ошибки изменения файлов и команд']]){
    const details=node('details'),summary=node('summary',title),body=node('div');details.append(summary,body);parent.append(details);let loaded=false,loading=false,generation=0;const cursors=[null];
    const button=(text,action)=>{const b=node('button',text,'button');b.type='button';b.onclick=action;return b;};
    async function load(){
      if(loading||!current())return;loading=true;const version=++generation;body.setAttribute('aria-busy','true');body.querySelectorAll('button').forEach(b=>b.disabled=true);
      try{
        const cursor=cursors[cursors.length-1],page=await api('/api/tasks/'+task.id+'/ledger/'+section+(cursor?'?before='+cursor:''));if(!current()||version!==generation)return;
        body.replaceChildren();loaded=true;
        if(page.historical_index_pending)body.append(node('p','Предыдущие действия ещё добавляются в журнал. Обновите раздел для продолжения.','form-help'));
        if(!page.items.length)body.append(node('p',section==='requirements'?'Агент ещё не выделил требования из запросов.':section==='requirement_checks'?'Проверок пока нет.':'В этом разделе ошибок нет.','form-help'));
        for(const item of page.items){
          const row=node('section',undefined,'ledger-row');body.append(row);
          if(section==='requirements'){
            row.append(node('h4',item.title),node('p',(labels[item.disposition]||item.disposition)+' · '+(labels[item.assessment]||item.assessment),'form-help'));
            if(item.criterion)row.append(node('p',item.criterion));
            if(item.note)row.append(node('p',item.note,'form-help'));
            const source=node('pre',item.source.preview,'ledger-source'),controls=node('div');row.append(source,controls);let offset=0;const offsets=[0];let sourceBusy=false;
            const show=async next=>{if(sourceBusy||!current())return;sourceBusy=true;open.disabled=true;try{const value=await api('/api/tasks/'+task.id+'/requirements/'+item.id+'/source?offset='+next);if(!current())return;offset=next;source.textContent=value.text;controls.replaceChildren();if(offsets.length>1)controls.append(button('Предыдущая часть',()=>{offsets.pop();show(offsets[offsets.length-1]);}));if(value.has_more)controls.append(button('Следующая часть',()=>{offsets.push(value.next_offset);show(value.next_offset);}));}catch(error){toast(error.message,true);}finally{sourceBusy=false;open.disabled=false;}};
            const open=button('Исходный фрагмент запроса',()=>{offsets.splice(0,offsets.length,0);show(0);});row.append(open);
          }else if(section==='requirement_checks'){
            const predicate={exit_code:'Код завершения',status:'Статус действия',mutation_verified:'Запись файла',artifact_contains:'Фрагмент результата',result_contains:'Фрагмент результата'}[item.predicate]||item.predicate;
            row.append(node('h4',predicate+' · '+(labels[item.verdict]||item.verdict)),node('p',item.freshness==='stale'?'После проверки задача или её файлы изменились.':'Проверка относится к сохранённому результату действия.','form-help'),node('pre','Ожидалось: '+JSON.stringify(item.expected)+'\nПолучено: '+JSON.stringify(item.observed),'ledger-source'));
            if(item.note)row.append(node('p',item.note,'form-help'));
            if(item.receipt.artifact)row.append(button('Открыть результат',()=>showArtifact(item.receipt.artifact)));
          }else{
            row.append(node('h4',toolTitle(item.name)+' · '+toolStatusTitle(item.status)),node('pre',item.result_preview,'ledger-source'));
            if(item.resolution)row.append(node('p','Агент связал с исправляющим действием: '+item.resolution.note,'form-help'));else row.append(node('p','Исправляющее действие не указано.','form-help'));
            if(item.result_artifact)row.append(button('Полный результат',()=>showArtifact(item.result_artifact)));
          }
        }
        const nav=node('div');if(cursors.length>1)nav.append(button('Назад',()=>{cursors.pop();load();}));if(page.has_more)nav.append(button('Следующая страница',()=>{cursors.push(page.next_before);load();}));nav.append(button('Обновить',load));body.append(nav);
      }catch(error){if(current()){body.replaceChildren(node('p',error.message,'form-help'),button('Повторить',load));}}finally{loading=false;body.removeAttribute('aria-busy');body.querySelectorAll('button').forEach(b=>b.disabled=false);}
    }
    details.ontoggle=()=>{if(details.open&&!loaded)load();};
  }
}
