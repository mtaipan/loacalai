'use strict';
function artifactLinks(tool){
  const links=[];
  for(const [field,label] of [['material_artifact','Исходный материал'],['result_artifact','Сохранённый результат'],['arguments_artifact','Полные параметры']]){
    const source=tool[field];if(!/^[0-9a-f-]{36}$/i.test(source?.id||''))continue;
    links.push(`<button class="button" data-artifact="${escapeHtml(source.id)}">${label}</button>`);
  }
  return links.length?`<div class="artifact-links">${tool.result_preview_only||tool.arguments_preview_only?'<p class="form-help">В карточке показана часть данных. Исходный результат сохранён отдельно.</p>':''}<div class="project-actions">${links.join('')}</div></div>`:'';
}
async function showArtifact(id){
  document.getElementById('artifact-dialog')?.close();
  const dialog=document.createElement('dialog');dialog.id='artifact-dialog';dialog.className='workspace-dialog';dialog.setAttribute('aria-labelledby','artifact-title');
  dialog.innerHTML='<h2 id="artifact-title">Сохранённый результат</h2><div class="dialog-body"><p class="artifact-caption form-help"></p><pre class="artifact-content" tabindex="0" aria-label="Сохранённый текст"></pre><p class="artifact-position form-help" role="status"></p><details><summary>Данные источника</summary><pre class="artifact-metadata"></pre></details></div><div class="dialog-footer"><button class="button artifact-previous">Назад</button><button class="button artifact-next">Дальше</button><a class="button artifact-download" download>Скачать текст</a><button class="button artifact-close">Закрыть</button></div>';
  document.body.append(dialog);const find=selector=>dialog.querySelector(selector);let offset=0,page=null,controller=null,version=0;
  dialog.onclose=()=>{controller?.abort();dialog.remove();};find('.artifact-close').onclick=()=>dialog.close();find('.artifact-download').href='/api/artifacts/'+id+'/download';
  async function render(){
    controller?.abort();controller=new AbortController();const generation=++version;find('.artifact-previous').disabled=true;find('.artifact-next').disabled=true;
    try{const value=await api('/api/artifacts/'+id+'?offset='+offset,{signal:controller.signal});if(!dialog.open||generation!==version)return;page=value;
      find('.artifact-content').textContent=page.text;find('.artifact-caption').textContent=(page.artifact.metadata?.tool_name?toolTitle(page.artifact.metadata.tool_name):'Текстовый материал')+' · сохранено '+new Date(page.artifact.created).toLocaleString('ru-RU');
      find('.artifact-position').textContent='Символы '+offset+'–'+page.next_offset+' из '+page.artifact.characters;
      find('.artifact-metadata').textContent=JSON.stringify(page.artifact,null,2);find('.artifact-previous').disabled=offset===0;find('.artifact-next').disabled=!page.has_more;
    }catch(error){if(error.name!=='AbortError'&&dialog.open)find('.artifact-position').textContent='Не удалось прочитать материал: '+error.message;}
  }
  find('.artifact-previous').onclick=()=>{offset=Math.max(0,offset-8000);render();};find('.artifact-next').onclick=()=>{offset=page?.next_offset||offset;render();};dialog.showModal();await render();
}

async function showApprovalPreview(id){
  const dialog=document.createElement('dialog');dialog.className='workspace-dialog';dialog.setAttribute('aria-label','Ввод в процесс');
  dialog.innerHTML='<h2>Ввод в процесс</h2><div class="dialog-body"><p class="form-help">Точный текст, включая переносы строк. Доступен до принятия решения.</p><pre class="artifact-content" tabindex="0"></pre><p class="form-help" role="status">Чтение…</p></div><div class="dialog-footer"><button class="button">Закрыть</button></div>';
  document.body.append(dialog);const controller=new AbortController();let timer;
  const erase=()=>{dialog.querySelector('pre').textContent='';dialog.close();};
  dialog.onclose=()=>{controller.abort();clearInterval(timer);dialog.remove();};dialog.querySelector('button').onclick=erase;dialog.showModal();
  try{const value=await api('/api/approve/'+id+'/preview',{signal:controller.signal});if(!dialog.open)return;dialog.querySelector('pre').textContent=value.text;dialog.querySelector('[role="status"]').textContent='';
    timer=setInterval(()=>{if(![...viewSessions.values()].some(session=>session.approvals.some(item=>item.id===id)))erase();},250);
  }catch(error){if(error.name!=='AbortError'&&dialog.open)dialog.querySelector('[role="status"]').textContent=error.message;}
}
