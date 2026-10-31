'use strict';
function webSettingsHtml(){return `<div class="settings-section"><h3>Интернет и браузер</h3><label class="form-label" for="search-provider">Поисковик</label><select class="full-input" id="search-provider"><option value="duckduckgo">DuckDuckGo + Bing резерв · без ключа</option><option value="searxng">SearXNG · свой сервер</option><option value="brave">Brave Search API · ключ</option></select><label class="form-label" for="searxng-url">Адрес SearXNG с включённым JSON</label><input class="full-input" id="searxng-url" type="url" placeholder="http://localhost:8080"><label class="form-label" for="brave-key">Ключ Brave Search API</label><input class="full-input" id="brave-key" type="password" autocomplete="off"><p class="form-help">Поисковые запросы передаются выбранному провайдеру; модель и история работают локально. При ограничении DuckDuckGo запрос передаётся Bing. Оба сервиса могут ограничивать автоматические запросы. Без интернета доступ к запущенным локальным сайтам остаётся. Ключ хранится в локальных настройках приложения.</p><button class="button" data-action="install-browser">Установить / проверить браузерные компоненты</button><label class="checkbox-row"><input id="browser-visible" type="checkbox">Показывать окно браузера для ручного входа и взаимодействия</label><p class="form-help">Применяется к новым браузерным сессиям. Для нового окна начните новый чат. Пароли можно вводить в самом окне браузера.</p><p class="form-help" id="browser-install-status">Используется отдельная сессия Edge/Chrome или Chromium; личный профиль браузера не подключается. Сессия сохраняется для продолжения в этом чате; закрывается при удалении чата, перезапуске или освобождении слота (до 4 сессий). Клики и ввод требуют подтверждения.</p></div>`;}
const thinkingCache=new Map();
let controlsModel='',thinkingChoice='off';
let webChoice=localStorage.getItem('gemma.webMode')||'auto';
const webLabels={off:'Выключена',auto:'Авто',search:'Поиск',research:'Исследование'};
const thoughtLabel=v=>v===false?'Выкл':v===true?'Вкл':({low:'Низкий',medium:'Средний',high:'Высокий',max:'Максимальный',xhigh:'Очень высокий'}[v]||v);
const thoughtValue=v=>v===false?'off':v===true?'on':String(v);
function controlMenu(id,label,items){return `<details class="tool-control" id="${id}"><summary>${label}<span aria-hidden="true">⌄</span></summary><div class="tool-control-options">${items}</div></details>`;}
function renderToolControls(values=[false],loading=false){
 const box=document.getElementById('tool-controls');if(!box)return;
 box.hidden=state.mode==='translate';
 const model=state.settings[modelField()];
 if(!loading&&thinkingChoice!=='auto'&&!values.some(v=>thoughtValue(v)===thinkingChoice))thinkingChoice=values.length===1?thoughtValue(values[0]):'auto';
 const reasoning=controlMenu('thinking-menu','Рассуждения: '+(loading?'…':thinkingChoice==='auto'?'Авто':thoughtLabel(thinkingChoice==='off'?false:thinkingChoice==='on'?true:thinkingChoice)),
  `<span class="tool-control-caption">${values.length===1&&values[0]===false?'Модель не поддерживает переключение':'Режимы выбранной модели'}</span>`+
  values.map(v=>`<button type="button" data-thinking="${escapeHtml(thoughtValue(v))}" ${state.busy||loading?'disabled':''}>${escapeHtml(thoughtLabel(v))}${thinkingChoice===thoughtValue(v)?icon('check'):''}</button>`).join('')+
  (values.length>1?`<button type="button" data-thinking="auto" ${state.busy?'disabled':''}>Авто · как задано моделью${thinkingChoice==='auto'?icon('check'):''}</button>`:''));
 const network=controlMenu('web-menu',icon('globe')+'Сеть: '+webLabels[webChoice],Object.entries(webLabels).map(([v,label])=>`<button type="button" data-web-mode="${v}" ${state.busy?'disabled':''}>${label}${v===webChoice?icon('check'):''}</button>`).join('')+'<span class="tool-control-caption">Поиск отправляет запрос поисковику. Локальные сайты доступны без интернета.</span>');
 box.innerHTML=network+reasoning;
 box.querySelectorAll('[data-thinking]').forEach(b=>b.onclick=()=>{thinkingChoice=b.dataset.thinking;localStorage.setItem('gemma.thinking.'+model,thinkingChoice);renderToolControls(values);});
 box.querySelectorAll('[data-web-mode]').forEach(b=>b.onclick=()=>{webChoice=b.dataset.webMode;localStorage.setItem('gemma.webMode',webChoice);renderToolControls(values);});
}
async function refreshToolControls(){
 const model=state.settings[modelField()];
 if(state.mode==='translate'){document.getElementById('tool-controls')?.setAttribute('hidden','');return;}
 if(controlsModel!==model){controlsModel=model;thinkingChoice=localStorage.getItem('gemma.thinking.'+model)||'off';}
 if(thinkingCache.has(model)){renderToolControls(thinkingCache.get(model));return;}
 renderToolControls([false],true);
 try{const r=await api('/api/thinking?model='+encodeURIComponent(model));thinkingCache.set(model,r.values);if(model===controlsModel)renderToolControls(r.values);}
 catch(error){if(model===controlsModel){renderToolControls([false]);const menu=document.getElementById('thinking-menu');if(menu)menu.title=error.message;}}
}
function reasoningHtml(text){return text?`<details class="reasoning"><summary>Рассуждения модели</summary><pre>${escapeHtml(text)}</pre></details>`:'';}
function browserToolTiming(tool){
 try{const r=JSON.parse(tool.result);if(Number.isFinite(r.execution_ms))return ' · '+Math.max(1,Math.round(r.execution_ms/1000))+' с'+(r.approval_wait_ms>=1000?' · ожидание подтверждения '+Math.round(r.approval_wait_ms/1000)+' с':'');}catch{}
 return tool.duration_ms!=null?' · '+Math.max(1,Math.round(tool.duration_ms/1000))+' с':'';
}
function sourcesHtml(tools){
 const sources=new Map();
 const screenshots=[];
 for(const tool of tools||[]){if(tool.status!=='succeeded'||!['web_search','fetch_url','browser_open','browser_read','browser_action'].includes(tool.name))continue;
  let r;try{r=JSON.parse(tool.result);}catch{continue;}
  if(r.screenshot?.id&&/^[0-9a-f-]{36}$/i.test(r.screenshot.id))screenshots.push(`<a class="browser-shot" href="/api/files/${r.screenshot.id}" target="_blank" rel="noopener noreferrer"><img src="/api/files/${r.screenshot.id}" alt="Снимок страницы браузера" loading="lazy"><span>Снимок страницы · открыть</span></a>`);
  const entries=tool.name==='web_search'?(r.results||[]):r.url?[{url:r.url,title:r.title||r.url,opened:true,status:r.status}]:[];
  for(const e of entries){try{const u=new URL(e.url);if(!['http:','https:'].includes(u.protocol))continue;const previous=sources.get(u.href);sources.set(u.href,{...previous,...e});}catch{}}
 }
 if(!sources.size)return screenshots.join('');
 return `<details class="web-sources"><summary>${icon('globe')}Источники · ${sources.size}</summary><div class="source-grid">${[...sources.values()].slice(0,20).map(e=>`<a href="${escapeHtml(e.url)}" target="_blank" rel="noopener noreferrer"><strong>${escapeHtml(e.title||e.url)}</strong><small>${escapeHtml(new URL(e.url).hostname)} · ${e.opened?'Открыта':'Найдена поиском'}${e.status?' · HTTP '+e.status:''}</small></a>`).join('')}</div></details>`+screenshots.join('');
}
async function installBrowserComponents(){
 const button=document.querySelector('[data-action="install-browser"]'),status=document.getElementById('browser-install-status');
 button.disabled=true;status.textContent='Установка компонентов… Браузер скачивается только при отсутствии Edge/Chrome. Файлы сохраняются в папке приложения.';
 try{const r=await api('/api/web/install',{method:'POST',body:{}});status.textContent=r.message||'Готово';}
 catch(error){status.textContent=error.message;}finally{button.disabled=false;}
}
document.addEventListener('click',event=>{if(!event.target.closest('.tool-control'))document.querySelectorAll('.tool-control[open]').forEach(d=>d.open=false);});
document.addEventListener('keydown',event=>{if(event.key==='Escape')document.querySelectorAll('.tool-control[open]').forEach(d=>d.open=false);});
