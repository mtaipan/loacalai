'use strict';
let modelMenuAll=false, modelMenuSaving=false;
const modelCaret=(direction='down')=>`<svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" aria-hidden="true"><path d="${direction==='right'?'m6 4 4 4-4 4':'m4 6 4 4 4-4'}"/></svg>`;

function closeModelMenu(restoreFocus=false){
  document.getElementById('model-menu')?.remove();
  const trigger=document.getElementById('model-name');
  trigger?.setAttribute('aria-expanded','false');
  if(restoreFocus)trigger?.focus();
}
function renderActiveModel(){
  const trigger=$('#model-name');if(!trigger)return;
  const name=modelName(state.settings[modelField()]);
  renderTeamMode();
  trigger.innerHTML=`<span>${escapeHtml(name)}</span>${modelCaret()}`;
  trigger.setAttribute('aria-label','Выбрать модель: '+name);
  trigger.disabled=state.busy||modelMenuSaving;
  if(trigger.disabled)closeModelMenu();
  if(typeof refreshToolControls==='function')refreshToolControls();
}
function positionModelMenu(){
  const menu=document.getElementById('model-menu');if(!menu)return;
  const rect=$('#model-name').getBoundingClientRect(),gap=8;
  menu.style.width=Math.min(320,innerWidth-24)+'px';
  menu.style.left=Math.max(12,Math.min(rect.right-menu.offsetWidth,innerWidth-menu.offsetWidth-12))+'px';
  const above=rect.top-12-gap,below=innerHeight-rect.bottom-12-gap;
  const upwards=above>=Math.min(menu.scrollHeight,340)||above>=below;
  menu.style.maxHeight=Math.max(80,Math.min(480,upwards?above:below))+'px';
  menu.style.top=upwards?'auto':rect.bottom+gap+'px';
  menu.style.bottom=upwards?innerHeight-rect.top+gap+'px':'auto';
}
function openModelMenu(all=false,focus=true){
  if(state.busy||modelMenuSaving)return;
  closeModelMenu();modelMenuAll=all;
  const role=state.mode==='translate'?'translate':'assistant',selected=state.settings[modelField()];
  const choices=state.catalog.filter(m=>m.role===role&&(state.mode!=='team'||m.id.toLowerCase().includes('qwen'))&&(all||hasModel(m.id)||m.id===selected));
  const menu=document.createElement('div');menu.id='model-menu';menu.className='model-popover';menu.setAttribute('role','menu');
  menu.setAttribute('aria-label',all?'Другие модели':'Выбор модели');
  const title=all?'Другие модели':state.mode==='team'?'Управляющая модель Qwen':state.mode==='translate'?'Модель перевода':state.mode==='agent'?'Модель агента':'Модель чата';
  menu.innerHTML=`<div class="model-popover-heading">${all?`<button class="model-back" data-model-back aria-label="Назад">${modelCaret('right')}</button>`:''}<span>${title}</span><span class="model-local">${state.localInference===false?"Внешний движок":"На устройстве"}</span></div><div class="model-popover-options">${choices.map(m=>{
    const ready=hasModel(m.id),active=m.id===selected;
    return `<button class="model-option ${active?'is-selected':''}" role="${ready?'menuitemradio':'menuitem'}" ${ready?`aria-checked="${active}" data-select-model="${escapeHtml(m.id)}"`:`data-install-model="${escapeHtml(m.id)}"`}><span class="model-option-copy"><span class="model-option-name">${escapeHtml(m.name)}</span>${!ready?'<span class="model-option-note">Скачать для использования</span>':''}</span><span class="model-option-trailing">${active?icon('check'):ready?'':icon('download')}</span></button>`;
  }).join('')||'<p class="model-empty">Пока нет установленных моделей</p>'}</div><div class="model-popover-footer">${all?`<button role="menuitem" data-model-settings>${icon('settings')}<span>Управление моделями</span>${modelCaret('right')}</button>`:`<button role="menuitem" data-model-more><span>Другие модели</span>${modelCaret('right')}</button>`}</div>`;
  document.body.append(menu);$('#model-name').setAttribute('aria-expanded','true');
  menu.addEventListener('click',async event=>{
    const button=event.target.closest('button');if(!button)return;
    if(button.hasAttribute('data-model-more'))return openModelMenu(true);
    if(button.hasAttribute('data-model-back'))return openModelMenu(false);
    if(button.hasAttribute('data-model-settings'))return openModelDownloads();
    if(button.dataset.installModel)return openModelDownloads(button.dataset.installModel);
    if(button.dataset.selectModel)await chooseActiveModel(button.dataset.selectModel);
  });
  menu.addEventListener('keydown',event=>{
    if(event.key==='Escape'){event.preventDefault();event.stopPropagation();return closeModelMenu(true);}
    const buttons=[...menu.querySelectorAll('button:not(:disabled)')],index=buttons.indexOf(document.activeElement);
    if(event.key==='ArrowDown'||event.key==='ArrowUp'){event.preventDefault();buttons[(index+(event.key==='ArrowDown'?1:-1)+buttons.length)%buttons.length]?.focus();}
    if(event.key==='Home'||event.key==='End'){event.preventDefault();buttons[event.key==='Home'?0:buttons.length-1]?.focus();}
    if(event.key==='ArrowLeft'&&modelMenuAll){event.preventDefault();openModelMenu(false);}
    if(event.key==='Tab')closeModelMenu();
  });
  positionModelMenu();
  if(focus)(menu.querySelector('.is-selected')||menu.querySelector('.model-option')||menu.querySelector('button'))?.focus({preventScroll:true});
}
async function chooseActiveModel(selected){
  const field=modelField();closeModelMenu(true);modelMenuSaving=true;renderActiveModel();
  try{
    const fresh=await api('/api/bootstrap');
    state.settings=await api('/api/settings',{method:'POST',body:{...fresh.settings,[field]:selected}});
  }catch(error){toast(error.message,true);}
  finally{modelMenuSaving=false;renderActiveModel();if(document.activeElement===document.body)$('#model-name').focus();}
}
async function openModelDownloads(id=null){
  closeModelMenu();await showSettings();
  const target=id?[...document.querySelectorAll('[data-download-model]')].find(b=>b.dataset.downloadModel===id):$('#models-status');
  const category=target?.closest('details.catalog-category');if(category)category.open=true;
  target?.scrollIntoView({block:'center',behavior:'smooth'});target?.focus({preventScroll:true});
}
function bindModelMenu(){
  $('#model-name').addEventListener('click',()=>document.getElementById('model-menu')?closeModelMenu():openModelMenu());
  $('#model-name').addEventListener('keydown',event=>{if(event.key==='ArrowDown'||event.key==='ArrowUp'){event.preventDefault();openModelMenu();}});
  uiListen(document,'pointerdown',event=>{if(!event.target.closest('#model-menu,#model-name'))closeModelMenu();});
  uiListen(document,'focusin',event=>{if(!event.target.closest('#model-menu,#model-name'))closeModelMenu();});
  uiListen(window,'resize',()=>{positionModelMenu();});
  uiListen(window,'scroll',event=>{if(!event.target.closest?.('#model-menu'))positionModelMenu();},true);
}
