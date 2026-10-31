'use strict';
function setupTeamUI(){
  const row=document.getElementById('agent-model-setting').closest('.form-row');
  row.insertAdjacentHTML('afterend',`<div class="team-settings"><label class="form-label" for="team-model-setting">Команда · управляющая модель Qwen</label><select id="team-model-setting"></select><p class="form-help">Qwen собирает данные и управляет инструментами. Foundation-Sec 8B анализирует переданные материалы и предлагает проверки. Обе модели нужно скачать в каталоге; автоматической загрузки нет. Модели работают по очереди и выгружаются после ответа. Это уменьшает удержание памяти, но увеличивает время переключения.</p></div>`);
  document.querySelector('.mode-row').insertAdjacentHTML('afterend','<p id="team-description" class="form-help team-description" hidden>Qwen — инструменты · Foundation-Sec — анализ безопасности. Заключение специалиста требует проверки.</p>');
}
function renderTeamMode(){
  const description=document.getElementById('team-description');
  if(description)description.hidden=state.mode!=='team';
  const trigger=document.getElementById('model-name');
  if(trigger)trigger.title=state.mode==='team'?'Выбор управляющей Qwen. Специалист: Foundation-Sec 8B.':'';
}
