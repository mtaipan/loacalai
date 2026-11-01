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
      const note=document.createElement('p');note.className='form-help';note.textContent='Отметки плана выставляет агент. Завершённый ответ не подтверждает готовность всей задачи.';content.append(note);
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
