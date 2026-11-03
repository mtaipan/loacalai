"use strict";
function messageTextLink(message,index){
  const metadata=['diagnostics','tools','attachments'].some(field=>message[field+'_partial']);
  const extra=metadata?`<p class="form-help">Часть сведений сообщения не показана. <a href="/api/chats/${encodeURIComponent(state.chat.id)}/export?format=json" download>Скачать все сообщения JSON</a></p>`:'';
  if(!message.content_partial)return extra;
  return `<p class="form-help">Показано начало: ${Number(message.content_source?.characters||0).toLocaleString('ru')} символов в сообщении.</p><button class="button" data-message-text="${index}">Читать весь текст</button>`+extra;
}
function messageDownloadURL(chat,message){
  return '/api/chats/'+encodeURIComponent(chat)+'/messages/'+encodeURIComponent(message.content_source.message)+'/download?'+new URLSearchParams({revision:message.content_source.revision});
}
function startDownload(url){const link=document.createElement('a');link.href=url;link.download='';document.body.append(link);link.click();link.remove();}
async function showMessageText(index){
  const chat=state.chat?.id,message=state.chat?.messages?.[index];if(!chat||!message?.content_source)return;
  document.getElementById('message-text-dialog')?.close();
  const opener=document.activeElement,dialog=document.createElement('dialog');dialog.id='message-text-dialog';dialog.className='workspace-dialog';dialog.setAttribute('aria-labelledby','message-text-title');
  dialog.innerHTML='<h2 id="message-text-title">Текст сообщения</h2><div class="dialog-body"><pre class="artifact-content" tabindex="0" aria-label="Часть сообщения"></pre><p class="form-help" role="status"></p></div><div class="dialog-footer"><button class="button text-previous">Назад</button><button class="button text-next">Дальше</button><button class="button text-copy">Копировать эту часть</button><button class="button text-insert">Вставить эту часть в запрос</button><a class="button text-download" download>Скачать весь текст</a><button class="button text-retry" hidden>Повторить чтение</button><button class="button text-close">Закрыть</button></div>';
  document.body.append(dialog);const find=s=>dialog.querySelector(s);let offset=0,page=null,controller=null,version=0,inserted=false;
  find('.text-download').href=messageDownloadURL(chat,message);
  dialog.onclose=()=>{controller?.abort();dialog.remove();if(inserted&&visibleConversation()===chat)focusComposer();else if(opener?.isConnected)opener.focus({preventScroll:true});};find('.text-close').onclick=()=>dialog.close();
  async function render(){
    controller?.abort();controller=new AbortController();const request=++version;const focus=document.activeElement;
    for(const s of ['.text-previous','.text-next','.text-copy','.text-insert'])find(s).disabled=true;
    find('.text-retry').hidden=true;find('[role="status"]').textContent='Чтение…';
    try{
      const query=new URLSearchParams({offset:String(offset),max_characters:'8000',revision:message.content_source.revision});
      const value=await api('/api/chats/'+encodeURIComponent(chat)+'/messages/'+encodeURIComponent(message.content_source.message)+'/content?'+query,{signal:controller.signal});
      if(!dialog.open||request!==version)return;page=value;find('pre').textContent=page.text;
      find('[role="status"]').textContent='Символы '+(page.characters?offset+1:0)+'–'+page.next_offset+' из '+page.characters;
      find('.text-previous').disabled=offset===0;find('.text-next').disabled=!page.has_more;find('.text-copy').disabled=false;find('.text-insert').disabled=visibleConversation()!==chat;
      if(focus===find('.text-previous')||focus===find('.text-next'))(focus.disabled?find('pre'):focus).focus({preventScroll:true});
    }catch(error){if(error.name==='AbortError'||!dialog.open||request!==version)return;find('pre').textContent='';page=null;find('[role="status"]').textContent='Не удалось прочитать: '+error.message;find('.text-retry').hidden=false;find('.text-retry').focus();}
  }
  find('.text-previous').onclick=()=>{offset=Math.max(0,offset-8000);render();};find('.text-next').onclick=()=>{offset=page?.next_offset||offset;render();};find('.text-retry').onclick=render;
  find('.text-copy').onclick=async()=>{if(!page)return;try{await copy(page.text);}catch(error){toast(error.message,true);}};
  find('.text-insert').onclick=()=>{if(!page||visibleConversation()!==chat)return;const destination=inputDestination();try{deliverInput(destination,page.text);inserted=true;dialog.close();}finally{releaseInput(destination);}};
  dialog.showModal();await render();
}
