'use strict';
const searchDepths={results:'Результаты',quick:'Быстрый',detailed:'Подробный',deep:'Глубокий'};
const searchFocuses={web:'Веб',news:'Новости',science:'Наука',docs:'Документация',discussions:'Обсуждения',materials:'Материалы задачи'};
function searchDefaults(){return {depth:'quick',focus:'web',period:'any',language:'',region:'',domains:[],excluded_domains:[],page:1,page_source:null,result_offset:0,after:0,snapshot:null};}
function searchOptions(){const value={...searchDefaults(),...((state.busy?activeRunOptions()?.search:currentSession().searchOptions)||{})};if(state.mode!=='search'&&value.depth==='results')value.depth='quick';return value;}
function searchControlOptions(values,current){return Object.entries(values).map(([value,label])=>`<option value="${value}" ${value===current?'selected':''}>${escapeHtml(label)}</option>`).join('');}
function setupSearchUI(){
 const row=document.querySelector('.mode-row');row.insertAdjacentHTML('afterend','<details id="search-preferences" class="search-preferences"><summary>Настройки поиска</summary><div id="search-preferences-body"></div></details>');
 document.getElementById('chat-model-setting').parentElement.insertAdjacentHTML('afterend','<div><label class="form-label" for="search-model-setting">Поиск · модель ответа</label><select id="search-model-setting"></select></div>');
 document.getElementById('search-preferences').addEventListener('change',event=>{
  const field=event.target.dataset.searchField;if(!field)return;const value=searchOptions();value[field]=['domains','excluded_domains'].includes(field)?event.target.value.split(/[\s,]+/).filter(Boolean):event.target.value.trim();value.page=1;value.page_source=null;value.result_offset=0;value.after=0;value.snapshot=null;currentSession().searchOptions=value;persistDraft();renderSearchMode();refreshToolControls();
 });
 document.addEventListener('click',event=>{const button=event.target.closest('[data-search-next]');if(!button)return;const message=state.chat?.messages.find(message=>message.id===button.dataset.searchNext);if(!message||state.busy)return;const d=message.diagnostics.search,result=d.result,options={...d.options};
  if(options.focus==='materials'){options.after=result.next_after;options.snapshot=result.snapshot;}
  else if(result.next_result_offset!=null){options.page_source=result.page_source.id;options.result_offset=result.next_result_offset;}
  else if(result.next_page!=null){options.page=result.next_page;options.page_source=null;options.result_offset=0;}else return;
  currentSession().searchOptions=options;setMode('search');document.getElementById('prompt').value=result.query||message.diagnostics.search.query||'';captureDraft();renderSearchMode();submitMessage();
 });
}
function renderSearchMode(){
 const box=document.getElementById('search-preferences');if(!box)return;box.hidden=state.mode==='translate';const body=document.getElementById('search-preferences-body'),value=searchOptions();
 // Preserve focus while a live request changes the surrounding UI.
 if(body.contains(document.activeElement))return;
 const stamp=JSON.stringify([value,state.mode,state.busy]);if(body.dataset.stamp===stamp)return;body.dataset.stamp=stamp;
 const disabled=state.busy?'disabled':'';const depths=state.mode==='search'?searchDepths:Object.fromEntries(Object.entries(searchDepths).filter(([key])=>key!=='results'));
 body.innerHTML=`<div class="form-row"><div><label class="form-label" for="search-depth">Глубина</label><select id="search-depth" data-search-field="depth" ${disabled}>${searchControlOptions(depths,value.depth)}</select></div><div><label class="form-label" for="search-focus">Источники</label><select id="search-focus" data-search-field="focus" ${disabled}>${searchControlOptions(searchFocuses,value.focus)}</select></div></div><div class="form-row"><div><label class="form-label" for="search-period">Период</label><select id="search-period" data-search-field="period" ${disabled}>${searchControlOptions({any:'Любой',day:'Сутки',week:'Неделя',month:'Месяц',year:'Год'},value.period)}</select></div><div><label class="form-label" for="search-language">Язык источников</label><input id="search-language" class="full-input" data-search-field="language" value="${escapeHtml(value.language)}" placeholder="ru, en" ${disabled}></div><div><label class="form-label" for="search-region">Страна</label><input id="search-region" class="full-input" data-search-field="region" value="${escapeHtml(value.region)}" placeholder="RU, US" ${disabled}></div></div><label class="form-label" for="search-domains">Только сайты · через пробел</label><input id="search-domains" class="full-input" data-search-field="domains" value="${escapeHtml(value.domains.join(' '))}" placeholder="docs.python.org" ${disabled}><label class="form-label" for="search-excluded">Исключить сайты</label><input id="search-excluded" class="full-input" data-search-field="excluded_domains" value="${escapeHtml(value.excluded_domains.join(' '))}" ${disabled}><p class="form-help">${value.focus==='materials'?'Поиск по сохранённым материалам этой задачи. Внешние запросы отключены.':value.depth==='results'?'Точный запрос отправляется поисковику. LLM и ранжирующая модель не запускаются.':'Запросы и страницы получает выбранный поисковик. Ответ составляет локальная модель. Глубина задаёт стратегию; фиксированного общего срока или числа страниц нет.'}</p>`;
 box.querySelector('summary').textContent=`Поиск: ${depths[value.depth]} · ${searchFocuses[value.focus]}`;
 const model=document.getElementById('model-name');if(state.mode==='search'&&value.depth==='results'&&model)model.textContent='Без модели';
}
function searchResultsHtml(message){
 const d=message.diagnostics?.search;if(d?.synthesis!==false||d.status!=='results')return '';
 const result=d.result||{},local=d.options.focus==='materials',entries=result.results||result.items||[];
 const cards=entries.map(entry=>{
  if(local)return `<article class="search-result"><p>${escapeHtml(entry.snippet||'')}</p>${entry.artifact_id?`<button class="button" data-artifact="${escapeHtml(entry.artifact_id)}">Открыть материал</button>`:''}</article>`;
  let url;try{url=new URL(entry.url);if(!['http:','https:'].includes(url.protocol))return '';}catch{return '';}
  return `<article class="search-result"><a href="${escapeHtml(url.href)}" target="_blank" rel="noopener noreferrer"><strong>${escapeHtml(entry.title||url.hostname)}</strong></a><small>${escapeHtml(url.hostname)} · найдена${entry.published?' · дата от провайдера: '+escapeHtml(entry.published):''}</small><p>${escapeHtml(entry.snippet||'')}</p></article>`;
 }).join('');
 const more=local?result.has_more:result.next_result_offset!=null||result.next_page!=null;
 return `<section class="search-results"><p class="form-help">${local?'Совпадения в материалах задачи':'Страница '+(result.page||1)+' · '+escapeHtml(result.provider||'')+' · страницы ещё не прочитаны'}. Анализ моделью не выполнялся.</p>${cards||'<p>Результатов нет.</p>'}${(result.warnings||[]).map(warning=>`<p class="form-help">${escapeHtml(warning)}</p>`).join('')}${more?`<button class="button" data-search-next="${escapeHtml(message.id)}">Следующая часть выдачи</button>`:''}<details><summary>Параметры выдачи</summary><pre>${escapeHtml(JSON.stringify({requested:d.options,effective:result.effective_filters},null,2))}</pre></details></section>`;
}
