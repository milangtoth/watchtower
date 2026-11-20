const managedRoot=document.getElementById("managed-portals");
async function request(path,method="GET",body){
 const response=await fetch(path,{method,headers:body?{"Content-Type":"application/json","X-Atlas-Request":"1"}:{},body:body?JSON.stringify(body):undefined});
 if(response.status===204)return null;
 const value=await response.json();if(!response.ok)throw Error(value.error||value.detail||"Request failed.");return value;
}
let refreshTimer;
async function showManaged(){
 clearTimeout(refreshTimer);
 if(!managedRoot)return;
 try{
  const portals=await request("/portals");
  managedRoot.innerHTML=portals.length?portals.map(p=>'<article class="source-card managed"><div class="source-top"><span class="src-tag">'+esc(p.state)+'</span><span>'+esc(p.tenderCount)+' tenders</span></div><h3>'+esc(p.url)+'</h3><p>'+esc(p.reason||("Last checked "+(p.lastSuccessAt||"not yet checked")) )+'</p><div class="source-actions"><button class="btn secondary" data-refresh="'+esc(p.id)+'">Refresh</button><button class="btn secondary" data-action="tenders" data-id="'+esc(p.id)+'">Tenders</button><button class="btn secondary" data-action="runs" data-id="'+esc(p.id)+'">Runs</button><button class="link" data-action="delete" data-id="'+esc(p.id)+'">Remove</button></div><pre hidden></pre></article>').join(""):'<p>No portals connected yet.</p>';
  if(portals.some(p => ["integrating","healing"].includes(p.state))) refreshTimer=setTimeout(showManaged,2500);
 }catch(e){managedRoot.textContent=e.message}
}
document.getElementById("portal-add-form")?.addEventListener("submit",async e=>{
 e.preventDefault();const form=e.currentTarget;const button=form.querySelector("button");button.disabled=true;
 try{await request("/portals","POST",{url:document.getElementById("managed-address").value.trim()});await showManaged();form.reset()}
 catch(err){alert(err.message)}finally{button.disabled=false}
});
managedRoot?.addEventListener("click",async e=>{
 const button=e.target.closest("button");if(!button)return;button.disabled=true;
 try{
  if(button.dataset.refresh){await request("/portals/"+button.dataset.refresh+"/refresh","POST",{});await showManaged()}
  else if(button.dataset.action==="delete"){await request("/portals/"+button.dataset.id,"DELETE");await showManaged()}
  else if(["tenders","runs"].includes(button.dataset.action)){const data=await request("/portals/"+button.dataset.id+"/"+button.dataset.action);const pre=button.closest("article").querySelector("pre");pre.hidden=false;pre.textContent=JSON.stringify(data,null,2); if(button.dataset.action==="tenders") appendDownloads(pre,data)}
 }catch(err){alert(err.message)}finally{button.disabled=false}
});



window.showManaged=showManaged;

function appendDownloads(container, tenders) {
 for (const tender of tenders) {
  for (const doc of tender.documents || []) {
   if (!/^\/files\/[a-f0-9]{64}$/.test(doc.path)) continue;
   const link = document.createElement("a");
   link.href = doc.path;
   link.download = doc.fileName;
   link.textContent = doc.fileName;
   container.append(document.createElement("br"), link);
  }
 }
}
