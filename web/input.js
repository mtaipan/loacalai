'use strict';
// Async input owns its destination until completion, independently of navigation.
function inputDestination(){
  captureDraft();const id=visibleConversation(),session=currentSession(id),prompt=document.getElementById('prompt');
  session.pendingInput=(session.pendingInput||0)+1;
  updateSend();
  return {id,session,text:session.draft,start:prompt.selectionStart,end:prompt.selectionEnd,released:false};
}
function releaseInput(target){
  if(target.released)return;target.released=true;target.session.pendingInput--;
  if(visibleConversation()===target.id)updateSend();else prunePreviousSession(target.id);
}
function deliverInput(target,text='',attachments=[]){
  const {id,session}=target,visible=visibleConversation()===id;
  if(visible)session.draft=document.getElementById('prompt').value;
  if(text){
    if(session.draft===target.text){session.draft=session.draft.slice(0,target.start)+text+session.draft.slice(target.end);target.start+=text.length;target.end=target.start;}
    else session.draft+=(session.draft&&!session.draft.endsWith('\n')?'\n':'')+text;
    target.text=session.draft;
  }
  session.pending.push(...attachments);persistDraft(id,session);
  if(visible){const prompt=document.getElementById('prompt');prompt.value=session.draft;resizeInput();renderPending();}
}
async function pasteNative(target){
  const own=!target;target ||= inputDestination();
  try{const result=await api('/api/clipboard',{method:'POST',body:{}});deliverInput(target,result.text||'',result.attachments||[]);if(!result.text&&!result.attachments?.length)toast('Буфер пуст или формат не поддерживается.');}
  finally{if(own)releaseInput(target);}
}
async function pasteClipboard(){
  const target=inputDestination();
  try{
    let items;
    try{
      if(!navigator.clipboard?.read){deliverInput(target,await navigator.clipboard.readText());return;}
      items=await navigator.clipboard.read();
    }catch{await pasteNative(target);return;}
    let pasted=false;
    for(const item of items){
      const image=item.types.find(type=>type.startsWith('image/'));
      if(image){const blob=await item.getType(image);await uploadFiles([new File([blob],'clipboard-'+Date.now()+'.'+image.split('/')[1],{type:image})],target);pasted=true;}
      else if(item.types.includes('text/plain')){deliverInput(target,await(await item.getType('text/plain')).text());pasted=true;}
    }
    if(!pasted)await pasteNative(target);
  }catch(error){toast('Не удалось вставить из буфера: '+error.message+'. Используйте Ctrl / ⌘ + V в поле сообщения.',true);}
  finally{releaseInput(target);}
}

let voiceView=null;
const currentVoice=voice=>state.voice===voice&&voiceView===voice.view&&document.getElementById('voice-dialog').open;
function openVoice(){
  if(voiceView)return;
  voiceView={target:inputDestination(),result:''};
  document.getElementById('voice-result').value='';document.getElementById('voice-result').classList.add('hidden');document.getElementById('use-voice').classList.add('hidden');
  document.getElementById('voice-state').textContent='Продиктуйте сообщение';document.getElementById('voice-time').textContent='00:00';resetVoiceControls();document.getElementById('voice-dialog').showModal();
}
function resetVoiceControls(){
  document.getElementById('voice-orb').classList.remove('recording');
  const record=document.getElementById('record-button');record.innerHTML=icon('mic')+'Начать запись';record.disabled=false;document.getElementById('system-record-button').disabled=false;
}
function timerVoice(voice){
  const started=Date.now();document.getElementById('voice-orb').classList.add('recording');
  voice.timer=setInterval(()=>{if(!currentVoice(voice))return;const seconds=Math.floor((Date.now()-started)/1000);document.getElementById('voice-time').textContent=String(Math.floor(seconds/60)).padStart(2,'0')+':'+String(seconds%60).padStart(2,'0');},300);
}
function voiceResult(voice,text){
  if(!currentVoice(voice))return;
  voice.view.result=text;document.getElementById('voice-result').value=text;document.getElementById('voice-result').classList.remove('hidden');document.getElementById('use-voice').classList.toggle('hidden',!text);document.getElementById('voice-state').textContent=text?'Речь распознана. Проверьте текст.':'Речь не обнаружена';
}
function finishVoice(voice){
  clearInterval(voice.timer);voice.stream?.getTracks().forEach(track=>track.stop());voice.chunks=[];
  if(state.voice===voice){state.voice=null;resetVoiceControls();}
}
function voiceFailure(voice,error){if(currentVoice(voice)&&!voice.abort.signal.aborted){toast(error.message,true);document.getElementById('voice-state').textContent='Не удалось распознать запись. Можно записать ещё раз.';}}
async function startRecord(){
  if(state.voice){const voice=state.voice;if(voice.phase==='recording'){voice.phase='transcribing';voice.recorder.stop();}else if(voice.job)await api('/api/stop/'+voice.job,{method:'POST',body:{}});return;}
  if(!voiceView)return;
  const voice={view:voiceView,phase:'requesting',abort:new AbortController(),chunks:[]};state.voice=voice;
  document.getElementById('record-button').disabled=true;document.getElementById('system-record-button').disabled=true;
  document.getElementById('voice-state').textContent='Ожидание доступа к микрофону…';
  try{
    if(!navigator.mediaDevices?.getUserMedia||!window.MediaRecorder)throw new Error('Запись в этом окне недоступна. Используйте «Системный микрофон».');
    const stream=await navigator.mediaDevices.getUserMedia({audio:true});voice.stream=stream;
    if(!currentVoice(voice)){finishVoice(voice);return;}
    const mime=['audio/webm;codecs=opus','audio/webm','audio/mp4'].find(type=>MediaRecorder.isTypeSupported(type)),recorder=new MediaRecorder(stream,mime?{mimeType:mime}:{});voice.recorder=recorder;
    recorder.ondataavailable=event=>{if(currentVoice(voice)&&event.data.size)voice.chunks.push(event.data);};
    recorder.onerror=event=>{voiceFailure(voice,event.error||new Error('Ошибка записи'));finishVoice(voice);};
    recorder.onstop=async()=>{
      clearInterval(voice.timer);stream.getTracks().forEach(track=>track.stop());
      if(!currentVoice(voice)){finishVoice(voice);return;}
      voice.phase='transcribing';document.getElementById('voice-orb').classList.remove('recording');document.getElementById('voice-state').textContent='Распознаём запись…';document.getElementById('record-button').disabled=true;
      try{
        const blob=new Blob(voice.chunks,{type:recorder.mimeType});voice.chunks=[];const form=new FormData();form.append('file',new File([blob],'voice-'+Date.now()+(recorder.mimeType.includes('mp4')?'.m4a':'.webm'),{type:blob.type}));
        const attachment=await api('/api/upload',{method:'POST',body:form,signal:voice.abort.signal});
        if(!currentVoice(voice))return;
        voice.job=crypto.randomUUID();
        const result=await api('/api/media',{method:'POST',body:{action:'audio',attachment:attachment.id,job:voice.job},signal:voice.abort.signal});voiceResult(voice,result.text||'');
      }catch(error){voiceFailure(voice,error);}finally{finishVoice(voice);}
    };
    recorder.start(1000);voice.phase='recording';timerVoice(voice);document.getElementById('voice-state').textContent='Запись идёт. Нажмите «Остановить», когда закончите.';document.getElementById('record-button').innerHTML=icon('stop')+'Остановить';document.getElementById('record-button').disabled=false;
  }catch(error){voiceFailure(voice,error);finishVoice(voice);}
}
async function systemRecord(){
  if(state.voice||!voiceView)return;
  const seconds=Number(localStorage.getItem('gemma.voiceDuration')||10),voice={view:voiceView,phase:'recording',job:crypto.randomUUID(),abort:new AbortController()};state.voice=voice;timerVoice(voice);
  document.getElementById('voice-state').textContent=`Системная запись · до ${seconds} секунд`;document.getElementById('record-button').innerHTML=icon('stop')+'Остановить';document.getElementById('system-record-button').disabled=true;
  try{const result=await api('/api/media',{method:'POST',body:{action:'record',seconds,job:voice.job},signal:voice.abort.signal});voiceResult(voice,result.text||'');}
  catch(error){voiceFailure(voice,error);}finally{finishVoice(voice);}
}
async function closeVoice(){
  const view=voiceView,voice=state.voice;voiceView=null;state.voice=null;
  document.getElementById('voice-dialog').close();
  if(view)releaseInput(view.target);
  if(voice){voice.abort.abort();voice.stream?.getTracks().forEach(track=>track.stop());if(voice.recorder?.state==='recording')voice.recorder.stop();clearInterval(voice.timer);voice.chunks=[];if(voice.job)try{await api('/api/stop/'+voice.job,{method:'POST',body:{}});}catch(error){toast('Не удалось подтвердить остановку распознавания: '+error.message,true);}}
}
async function useVoice(){if(!voiceView)return;deliverInput(voiceView.target,document.getElementById('voice-result').value);await closeVoice();}
