'use strict';
let responseTimer=null,responseStarted=0;
function formatResponseDuration(ms){
  if(ms<1000)return '< 1 с';
  const seconds=Math.floor(ms/1000),hours=Math.floor(seconds/3600),minutes=Math.floor(seconds%3600/60);
  return [hours?hours+' ч':'',minutes?minutes+' мин':'',seconds%60+' с'].filter(Boolean).join(' ');
}
function responseTimingHtml(message){
  if(message.role!=='assistant'||!Number.isFinite(message.duration_ms)||message.duration_ms<0)return '';
  return `<details class="response-timing"><summary>Работал ${escapeHtml(formatResponseDuration(message.duration_ms))}<svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" aria-hidden="true"><path d="m6 4 4 4-4 4"/></svg></summary><p>Общее время запроса: подготовка, обработка вложений, генерация и работа инструментов. Ожидание подтверждений тоже учитывается.</p></details>`;
}
function startResponseTimer(){
  stopResponseTimer();responseStarted=performance.now();
  const update=()=>{const label=document.getElementById('stream-duration');if(label)label.textContent='Работаю · '+formatResponseDuration(performance.now()-responseStarted);};
  update();responseTimer=setInterval(update,1000);
}
function stopResponseTimer(){clearInterval(responseTimer);responseTimer=null;}
