'use strict';
const modelDirectories=new Map();
function renderModelCatalog(){
 const groups=new Map();
 for(const m of [...state.catalog].sort((a,b)=>Number(!!b.recommended)-Number(!!a.recommended))){const category=m.recommended?'Рекомендуемые':m.category==='Translation'?'Перевод':m.category==='Chat and agent'?'Чат и агент':m.category||'Другие';if(!groups.has(category))groups.set(category,[]);groups.get(category).push(m);}
 const installedPath=m=>(state.status?.models||[]).find(x=>x.name===m.id||x.model===m.id)?.storage_path;
 const defaultPath=state.status?.models_path||'';
 return `<p class="form-help">Для каждой модели можно указать свою папку на любом диске. Пустое поле — ${escapeHtml(defaultPath)}. Установленную модель сначала удалите, чтобы скачать в другое место. Веса не входят в установщик.</p>`+
 [...groups].map(([category,models])=>`<section class="catalog-category"><h3>${escapeHtml(category)}</h3>${models.map(m=>`<div class="catalog-model"><div><strong>${escapeHtml(m.name)}</strong><span class="tag">${escapeHtml(m.size)} · ${hasModel(m.id)?'установлена':'не установлена'}</span><p>${escapeHtml(m.description)}</p>${m.source?`<a href="${escapeHtml(m.source)}" target="_blank" rel="noopener noreferrer">Источник модели ↗</a>`:''}<label class="form-label">Папка модели<input class="full-input model-directory" data-model-directory="${escapeHtml(m.id)}" value="${escapeHtml(modelDirectories.get(m.id)??installedPath(m)??state.status?.model_locations?.[m.id]??'')}" placeholder="${escapeHtml(defaultPath)}" ${hasModel(m.id)||state.downloading?'disabled':''}></label></div>${hasModel(m.id)?`<button class="button danger" data-delete-model="${escapeHtml(m.id)}" ${state.downloading||state.busy?'disabled':''}>Удалить</button>`:`<button class="button" data-download-model="${escapeHtml(m.id)}" ${state.downloading?'disabled':''}>Скачать</button>`}</div>`).join('')}</section>`).join('')+
 (state.status?.storage_errors||[]).map(e=>`<p class="form-help">${escapeHtml(e.path)}: ${escapeHtml(e.error)}</p>`).join('');
}
document.addEventListener('input',e=>{if(e.target.dataset.modelDirectory)modelDirectories.set(e.target.dataset.modelDirectory,e.target.value);});
