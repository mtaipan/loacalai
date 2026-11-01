'use strict';
let settingsDirty=false,settingsLoaded=false,settingsSaving=false,settingsRevision=0,settingsEditVersion=0;
let settingsCategory='general';
const toolLabels={read_artifact:'Чтение сохранённого материала',search_artifacts:'Поиск в материалах задачи',start_process:'Запуск процесса',poll_process:'Вывод процесса',write_stdin:'Ввод в процесс',stop_process:'Остановка процесса',read_task:'Цель и состояние задачи',update_plan:'План задачи',web_search:'Поиск в интернете',fetch_url:'Чтение страницы',browser_open:'Открытие страницы',browser_read:'Просмотр страницы',browser_action:'Действие на сайте',inspect_project:'Обзор проекта',git_info:'Состояние Git',list_files:'Список файлов',read_file:'Чтение файла',find_files:'Поиск файлов',search_files:'Поиск по содержимому',write_file:'Запись файла',edit_file:'Изменение файла',copy_file:'Копирование файла',move_file:'Перемещение файла',delete_file:'Удаление файла',translate_text:'Перевод текста',translate_file:'Перевод файла',read_document:'Чтение документа',read_image:'Распознавание текста на изображении',transcribe_audio:'Распознавание речи',record_microphone:'Запись микрофона',run_command:'Команда',security_analyze:'Анализ Foundation-Sec',get_system_paths:'Папки пользователя'};
function toolTitle(name){return toolLabels[name]||name;}
function toolStatusTitle(status){return {running:'Выполняется',awaiting_approval:'Ожидает подтверждения',succeeded:'Выполнено',denied:'Отклонено',expired:'Время истекло',cancelled:'Остановлено',failed:'Ошибка',skipped:'Пропущено',timed_out:'Истёк срок',needs_input:'Нужны данные',legacy:'Завершено · старый формат'}[status]||'Статус неизвестен';}
let settingsFormCache=null;
let uiEvents=new AbortController();
function uiListen(target,type,handler,options={}){target.addEventListener(type,handler,{...(typeof options==='boolean'?{capture:options}:options),signal:uiEvents.signal});}
function resetUI(){
  if(settingsDirty&&document.getElementById('settings-dialog'))settingsFormCache=[...document.querySelectorAll('#settings-dialog input[id],#settings-dialog select[id],#settings-dialog textarea[id]')].map(input=>({id:input.id,value:input.value,checked:input.checked,options:input.tagName==='SELECT'?[...input.options].map(option=>({value:option.value,text:option.text,disabled:option.disabled})):null}));
  else{settingsFormCache=null;settingsLoaded=false;}
  document.querySelectorAll('.workspace-dialog').forEach(dialog=>{dialog.close();dialog.remove();});
  uiEvents.abort();uiEvents=new AbortController();document.getElementById('sidebar-backdrop')?.remove();clearInterval(state.statusTimer);clearInterval(responseTimer);clearTimeout(renderTimer);renderTimer=null;closeModelMenu();
}
function showSettingsCategory(id){
  settingsCategory=id;
  document.querySelectorAll('[data-settings-tab]').forEach(button=>{const active=button.dataset.settingsTab===id;button.setAttribute('aria-selected',active);button.tabIndex=active?0:-1;});
  document.querySelectorAll('[data-settings-page]').forEach(page=>page.hidden=page.dataset.settingsPage!==id);
}
function setupSettingsPages(){
  const dialog=document.getElementById('settings-dialog'),body=dialog.querySelector('.dialog-body');
  const categories=[['general','Общее'],['models','Модели'],['agent','Агент'],['permissions','Права'],['network','Интернет'],['compute','Вычисления'],['quick','Быстрый перевод'],['context','Контекст']];
  const nav=document.createElement('div');nav.className='settings-nav';nav.setAttribute('role','tablist');nav.setAttribute('aria-label','Разделы настроек');
  const pages=document.createElement('div');pages.className='settings-pages';
  const panels=new Map();
  for(const[id,label]of categories){
    const button=document.createElement('button');button.type='button';button.textContent=label;button.dataset.settingsTab=id;button.id='settings-tab-'+id;button.setAttribute('role','tab');button.setAttribute('aria-controls','settings-page-'+id);button.onclick=()=>showSettingsCategory(id);nav.append(button);
    const panel=document.createElement('section');panel.id='settings-page-'+id;panel.dataset.settingsPage=id;panel.setAttribute('role','tabpanel');panel.setAttribute('aria-labelledby',button.id);pages.append(panel);panels.set(id,panel);
  }
  const footer=body.querySelector('.dialog-footer');footer.remove();
  for(const child of [...body.children]){
    let category='general';
    const has=selector=>child.matches(selector)||!!child.querySelector(selector);
    if(has('#permission-mode'))category='permissions';
    else if(has('#compute-device'))category='compute';
    else if(has('#context-window'))category='context';
    else if(has('#translation-model-setting,#models-status'))category='models';
    else if(has('#agent-instructions,#agent-actions-setting'))category='agent';
    else if(has('#search-provider,#reranker-enabled'))category='network';
    else if(has('#quick-hotkey-setting'))category='quick';
    panels.get(category).append(child);
  }
  body.replaceChildren(nav,pages);dialog.append(footer);
  const status=document.createElement('span');status.id='settings-dirty';status.setAttribute('role','status');footer.prepend(status);
  const discard=document.createElement('button');discard.type='button';discard.className='button';discard.textContent='Сбросить изменения';
  discard.onclick=()=>{settingsDirty=false;settingsLoaded=false;dialog.close();showSettings().catch(error=>toast(error.message,true));};footer.insertBefore(discard,footer.lastElementChild);
  uiListen(dialog,'input',()=>{settingsEditVersion++;settingsDirty=true;status.textContent='Есть несохранённые изменения';});
  uiListen(dialog,'change',()=>{settingsEditVersion++;settingsDirty=true;status.textContent='Есть несохранённые изменения';});
  uiListen(nav,'keydown',event=>handleTabKeys(event,nav,button=>showSettingsCategory(button.dataset.settingsTab)));
  showSettingsCategory(settingsCategory);
  if(settingsFormCache){for(const saved of settingsFormCache){const input=document.getElementById(saved.id);if(!input)continue;if(saved.options)input.replaceChildren(...saved.options.map(value=>{const option=new Option(value.text,value.value);option.disabled=value.disabled;return option;}));input.value=saved.value;if(saved.checked!==undefined)input.checked=saved.checked;}status.textContent='Есть несохранённые изменения';settingsLoaded=true;}
}
function handleTabKeys(event,container,select){
  if(!['ArrowLeft','ArrowRight','Home','End'].includes(event.key))return;
  const tabs=[...container.querySelectorAll('[role="tab"]:not(:disabled)')],index=tabs.indexOf(event.target);if(index<0)return;
  event.preventDefault();const next=event.key==='Home'?0:event.key==='End'?tabs.length-1:(index+(event.key==='ArrowRight'?1:-1)+tabs.length)%tabs.length;
  select(tabs[next]);tabs[next].focus();
}
function validSettings(){
  const invalid=[...document.querySelectorAll('#settings-dialog input,#settings-dialog select,#settings-dialog textarea')].find(input=>!input.checkValidity());
  if(!invalid)return true;const panel=invalid.closest('[data-settings-page]');if(panel)showSettingsCategory(panel.dataset.settingsPage);invalid.reportValidity();invalid.focus();return false;
}
async function saveSettings(){
  if(settingsSaving||!validSettings())return;settingsSaving=true;
  const version=settingsEditVersion,value=settingsFormValue(),voiceDuration=document.getElementById('voice-duration').value;
  const buttons=[...document.querySelectorAll('#settings-dialog>.dialog-footer button')];buttons.forEach(button=>button.disabled=true);
  try{await persistSettings(value,voiceDuration);if(version===settingsEditVersion){settingsDirty=false;settingsLoaded=false;document.getElementById('settings-dirty').textContent='';document.getElementById('settings-dialog').close();toast('Настройки сохранены');}else{settingsDirty=true;document.getElementById('settings-dirty').textContent='Сохранено; есть новые изменения';}}
  finally{settingsSaving=false;buttons.forEach(button=>button.disabled=false);}
}
function handleGlobalShortcut(event){
  if(event.isComposing)return;
  const dialog=document.querySelector('dialog[open]');
  if(dialog)return;
  if(event.key==='Escape'){
    if(document.getElementById('model-menu')){event.preventDefault();closeModelMenu(true);return;}
    const menu=document.querySelector('.context-menu');if(menu){event.preventDefault();menu.remove();return;}
    const details=document.querySelector('#tool-controls details[open]');if(details){event.preventDefault();details.open=false;details.querySelector('summary')?.focus();return;}
    if(innerWidth<761&&!document.body.classList.contains('sidebar-hidden')){event.preventDefault();setSidebarOpen(false);return;}
    const panel=document.getElementById('file-panel');if(panel.classList.contains('visible')){event.preventDefault();panel.classList.remove('visible');document.querySelector('[data-action="files"]')?.focus();}return;
  }
  const editable=event.target.closest('input,textarea,[contenteditable="true"]');
  if(!editable&&(event.ctrlKey||event.metaKey)&&event.key.toLowerCase()==='k'){event.preventDefault();setSidebarOpen(true);document.getElementById('chat-search').focus();}
  if(!editable&&(event.ctrlKey||event.metaKey)&&event.key.toLowerCase()==='n'){event.preventDefault();newChat();}
}
function setSidebarOpen(open){
  document.body.classList.toggle('sidebar-hidden',!open);
  const mobile=innerWidth<761;
  const sidebar=document.querySelector('.sidebar');
  if(mobile&&open){sidebar.setAttribute('role','dialog');sidebar.setAttribute('aria-modal','true');sidebar.setAttribute('aria-label','Чаты');}else{sidebar.removeAttribute('role');sidebar.removeAttribute('aria-modal');sidebar.removeAttribute('aria-label');}
  document.getElementById('sidebar-backdrop').hidden=!open||!mobile;
  document.querySelectorAll('main,.file-panel').forEach(element=>element.inert=mobile&&open);
  document.querySelector('[data-action="sidebar"]').setAttribute('aria-expanded',String(open));
  if(open&&mobile)document.getElementById('chat-search').focus({preventScroll:true});
  if(!open)document.querySelector('[data-action="sidebar"]')?.focus({preventScroll:true});
}
function setupUI(){
  for(const dialog of document.querySelectorAll('dialog')){
    const title=dialog.querySelector('h2');if(title){title.id=title.id||dialog.id+'-title';dialog.setAttribute('aria-labelledby',title.id);}
  }
  setupSettingsPages();
  const backdrop=document.createElement('button');backdrop.type='button';backdrop.id='sidebar-backdrop';backdrop.className='sidebar-backdrop';backdrop.hidden=true;backdrop.setAttribute('aria-label','Закрыть список чатов');backdrop.onclick=()=>setSidebarOpen(false);document.body.append(backdrop);
  const sidebar=document.querySelector('.sidebar');uiListen(sidebar,'keydown',event=>{
    if(innerWidth>=761||document.body.classList.contains('sidebar-hidden')||event.key!=='Tab')return;
    const items=[...sidebar.querySelectorAll('button,input,a[href]')].filter(element=>!element.disabled&&element.getClientRects().length);
    if(event.shiftKey&&document.activeElement===items[0]){event.preventDefault();items.at(-1)?.focus();}else if(!event.shiftKey&&document.activeElement===items.at(-1)){event.preventDefault();items[0]?.focus();}
  });
  uiListen(document.querySelector('.mode-tabs'),'keydown',event=>handleTabKeys(event,event.currentTarget,button=>{if(!state.busy)setMode(button.dataset.mode);}));
  uiListen(matchMedia('(prefers-color-scheme: light)'),'change',()=>{if(state.settings.theme==='system')setTheme();});
  uiListen(matchMedia('(max-width: 760px)'),'change',()=>setSidebarOpen(false));
}
