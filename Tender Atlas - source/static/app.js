"use strict";
const $ = id => document.getElementById(id);
const icons = {
  sync:"M20 7v5h-5 M4 17v-5h5 M6 8a7 7 0 0 1 12-2l2 3 M18 16a7 7 0 0 1-12 2l-2-3",
  search:"M21 21l-5-5 M18 10.5a7.5 7.5 0 1 1-15 0 7.5 7.5 0 0 1 15 0",
  arrow:"M4 12h15 M13 6l6 6-6 6", diagonal:"M5 19 19 5 M7 5h12v12",
  layers:"m3 8 9-5 9 5-9 5z M3 12l9 5 9-5 M3 16l9 5 9-5",
  globe:"M22 12a10 10 0 1 1-20 0 10 10 0 0 1 20 0 M2 12h20 M12 2c-6 5-6 15 0 20 6-5 6-15 0-20",
  database:"M20 6c0 4-16 4-16 0s16-4 16 0z M4 6v12c0 4 16 4 16 0V6 M4 12c0 4 16 4 16 0",
  calendar:"M3 7h18v14H3z M3 10h18 M7 3v5 M17 3v5",
  download:"M12 3v12 M7 10l5 5 5-5 M4 16v5h16v-5",
  down:"m6 9 6 6 6-6", close:"M5 5l14 14 M19 5 5 19",
  tag:"M3 3h8l10 10-8 8L3 11z M7 7h.01", sort:"M3 6h17 M3 12h12 M3 18h7",
  next:"m9 5 7 7-7 7", prev:"m15 5-7 7 7 7"
};
function icon(name){return '<svg aria-hidden="true" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round"><path d="'+icons[name]+'"/></svg>'}
for(const el of document.querySelectorAll("i[data-icon]"))el.innerHTML=icon(el.dataset.icon);
function esc(x){return String(x??"").replace(/[&<>"']/g,ch=>({"&":"&amp;","<":"&lt;",">":"&gt;",'"':"&quot;","'":"&#39;"}[ch]))}
function external(url,label,cls=""){try{const u=new URL(url);if(!["https:","http:"].includes(u.protocol))return "";return '<a class="'+cls+'" href="'+esc(u.href)+'" target="_blank" rel="noopener noreferrer">'+esc(label)+(cls.includes("btn")?icon("diagonal"):"")+'</a>'}catch{return ""}}
const fmt=new Intl.NumberFormat("en-GB");
const dateFmt=new Intl.DateTimeFormat("en-GB",{day:"2-digit",month:"short",year:"numeric",timeZone:"UTC"});
function dateText(s){const v=String(s||"").slice(0,10);return /^\d{4}-\d{2}-\d{2}$/.test(v)?dateFmt.format(new Date(v+"T12:00:00Z")):"—"}
function stageTag(s){return '<span class="stage s-'+esc(s.toLowerCase().replace(/[^a-z]/g,""))+'">'+esc(s)+'</span>'}
async function api(path,body){const opts=body===undefined?{}:{method:"POST",headers:{"Content-Type":"application/json","X-Atlas-Request":"1"},body:JSON.stringify(body)};const response=await fetch(path,opts);const data=await response.json();if(!response.ok)throw Error(data.error||"Request failed.");return data}
let stats, job=null, view="explorer", modal="", page=1, searchText="", serial=0, noticeSerial=0, lastRunSignature="", toastTimer;
function toast(msg){clearTimeout(toastTimer);$("toast").textContent=msg;$("toast").hidden=false;toastTimer=setTimeout(()=>$("toast").hidden=true,4500)}
function query(){
 const p=new URLSearchParams();for(const id of ["country","source","stage","sort","from","to"])if($(id).value)p.set(id,$(id).value);
 if($("upcoming").checked)p.set("upcoming","1");if(searchText)p.set("q",searchText);return p
}
function options(id,pairs,label){const v=$(id).value;$(id).innerHTML='<option value="">'+label+'</option>'+pairs.map(([k,n])=>'<option value="'+esc(k)+'">'+esc(n)+'</option>').join("");$(id).value=v}
async function readStats(){
 stats=await api("/api/stats");$("m-total").textContent=fmt.format(stats.total);$("m-geo").textContent=fmt.format(stats.countries.filter(v=>v!=="Unspecified").length);
 $("m-sources").textContent=Object.keys(stats.sources).filter(k=>k!=="portal").length;$("m-latest").textContent=dateText(stats.latest);
 options("country",stats.countries.map(v=>[v,v]),"All countries");options("stage",stats.stages.map(v=>[v,v]),"All notice types");
 options("source",Object.entries(stats.sources).map(([k,v])=>[k,v.short]),"All sources");
 $("source-grid").innerHTML=Object.entries(stats.sources).map(([k,s])=>'<article class="source-card"><div class="source-top"><span class="src-tag">'+esc(s.short)+'</span><span class="region">'+esc(s.region)+'</span></div><h3>'+esc(s.name)+'</h3><p>'+esc(s.description)+'</p><div class="source-end"><strong>'+fmt.format(s.count)+' indexed</strong><br>'+(s.count?dateText(s.first)+' – '+dateText(s.last):'Awaiting first collection')+'<br>'+external(s.url,'Publisher information ↗')+'</div></article>').join("")
}
async function results(scroll=false){
 const n=++serial;const params=query();params.set("page",page);$("records").classList.add("busy");
 try{const data=await api("/api/notices?"+params);if(n!==serial)return;page=data.page;
 $("result-info").textContent=fmt.format(data.total)+(data.total===1?" matching notice":" matching notices")+(searchText?' for “'+searchText+'”':"");
 $("reset").hidden=![...params].some(([k])=>!["sort","page"].includes(k));
 const x=query();$("csv-link").href="/api/export?"+x;x.set("format","jsonl");$("raw-link").href="/api/export?"+x;
 if(data.rows.length){
  $("records").innerHTML='<table><thead><tr><th>Notice / Buyer</th><th>Country</th><th>Notice type</th><th>Source</th><th>Record date</th><th></th></tr></thead><tbody>'+data.rows.map(r=>'<tr data-id="'+r.id+'"><td><button class="record-title">'+esc(r.title)+'</button><div class="record-buyer">'+esc(r.buyer||"Buyer unspecified")+'</div></td><td>'+esc(r.country)+'</td><td>'+stageTag(r.stage)+'</td><td><span class="src-tag">'+esc(stats.sources[r.source].short)+'</span></td><td class="nowrap">'+dateText(r.published)+'</td><td class="arrow-cell">'+icon("next")+'</td></tr>').join("")+'</tbody></table>';
 }else{$("records").innerHTML='<div class="empty"><strong>No matching notices</strong>'+(stats.total?'Try another keyword or clear a filter.':'Use Collect notices to build your index.')+'</div>'}
 const pages=Math.max(1,Math.ceil(data.total/data.limit));
 $("pagination").innerHTML=data.total?'<span>Page '+fmt.format(page)+' of '+fmt.format(pages)+'</span><div><button class="btn secondary" data-page="-1" '+(page<=1?'disabled':'')+'>Previous</button><button class="btn secondary" data-page="1" '+(page>=pages?'disabled':'')+'>Next</button></div>':'';
 if(scroll)$("explorer").scrollIntoView({block:"start"});
 }catch(e){if(n===serial)$("records").innerHTML='<p class="empty">'+esc(e.message)+'</p>'}finally{if(n===serial)$("records").classList.remove("busy")}
}
function setView(v){view=v;$("discover").hidden=v!=="discover";$("explorer").hidden=v!=="explorer";$("sources").hidden=v!=="sources";$("portals").hidden=v!=="portals";for(const b of document.querySelectorAll("[data-view]")){b.classList.toggle("active",b.dataset.view===v);b.setAttribute("aria-current",b.dataset.view===v?"page":"false")}history.replaceState(null,"","#"+v);if(v==="portals")window.showManaged?.()}
for(const b of document.querySelectorAll("[data-view]"))b.onclick=()=>setView(b.dataset.view);
document.querySelector(".brand").onclick=e=>{e.preventDefault();setView("explorer");window.scrollTo(0,0)};
$("search-form").onsubmit=e=>{e.preventDefault();searchText=$("query").value.trim();page=1;setView("explorer");results()};
$("query").onsearch=()=>{if(!$("query").value){searchText="";page=1;results()}};
for(const id of ["country","source","stage","sort","from","to","upcoming"])$(id).onchange=()=>{page=1;results()};
$("reset").onclick=()=>{for(const i of ["country","source","stage","from","to","query"])$(i).value="";$("upcoming").checked=false;searchText="";page=1;results()};
$("dates-toggle").onclick=()=>{const d=$("date-filters");d.hidden=!d.hidden;$("dates-toggle").setAttribute("aria-expanded",!d.hidden)};
$("export-toggle").onclick=()=>{const el=$("export-menu");el.hidden=!el.hidden;$("export-toggle").setAttribute("aria-expanded",!el.hidden)};
document.addEventListener("click",e=>{if(!e.target.closest(".export-wrap")||e.target.closest(".menu a")){$("export-menu").hidden=true;$("export-toggle").setAttribute("aria-expanded","false")}});
$("pagination").onclick=e=>{const b=e.target.closest("[data-page]");if(b&&!b.disabled){page+=Number(b.dataset.page);results(true)}};
function dialog(kind,kicker,html){modal=kind;$("dlg-kicker").textContent=kicker;$("dialog-body").innerHTML=html;if(!$("drawer").open)$("drawer").showModal();$("drawer").scrollTop=0}
$("close").onclick=()=>$("drawer").close();
$("drawer").addEventListener("close",()=>{modal="";noticeSerial++});
$("drawer").addEventListener("click",e=>{if(e.target===$("drawer") && e.clientX<$("drawer").getBoundingClientRect().left)$("drawer").close()});
function metadata(arr){return '<dl>'+arr.filter(x=>x[1]!==""&&x[1]!=null).map(([k,v])=>'<dt>'+esc(k)+'</dt><dd>'+esc(v)+'</dd>').join("")+'</dl>'}
function provenancePanel(p){
 const evidence={captured:"Recorded during collection", verified_by_refetch:"Matched to a later publisher response", verified_from_archive:"Verified against the saved publisher file", legacy_url_only:"Historical import · only the source URL was recorded"};
 const req=p.request, loc=p.record_locator||{};
 const position=loc.pointer||loc.xpath||loc.url||(loc.one_based_row?"Data row "+loc.one_based_row+" (header excluded)":"");
 const fields=[["Evidence",evidence[p.evidence]],["Source URL",req.url],["Website path",req.path],["Query string",req.query],["Request method",req.method],
 ["Resolved URL",p.response_url&&p.response_url!==req.url?p.response_url:""],["Discovered on",p.discovered_from?.url],
 ["Archive member",loc.archive_member],["Record location",position],["Response SHA-256",p.response_sha256]];
 return '<section class="trace-panel"><h3>Source provenance</h3>'+metadata(fields)+'<details><summary>View machine-readable provenance</summary><pre>'+esc(JSON.stringify(p,null,2))+'</pre></details></section>';
}
async function openNotice(id){
 const token=++noticeSerial;dialog("notice","Notice details",'<p>Loading notice…</p>');
 try{const r=await api("/api/notice/"+id);if(modal!=="notice"||token!==noticeSerial)return;
 const val=r.amount?((Number.isFinite(Number(r.amount))?fmt.format(Number(r.amount)):r.amount)+" "+r.currency).trim():"";
 const source=stats.sources[r.source];
 dialog("notice",source.short+" / "+r.country,stageTag(r.stage)+'<h2 class="detail-title">'+esc(r.title)+'</h2><p class="dialog-lead">'+esc(r.buyer)+'</p><div class="source-actions">'+external(r.url,"Open official record","btn primary")+(r.source==="ted"?external("https://ted.europa.eu/en/notice/"+encodeURIComponent(r.ref)+"/xml","Full XML","btn secondary"):"")+'</div>'+
 metadata([["Record date",dateText(r.published)],["Deadline(s)",r.deadline],[(r.value_type||"Value"),val],["Supplier(s)",r.supplier],["CPV codes",r.cpv],["Procedure ID",r.process],["Source reference",r.ref],["Publisher",source.name],["Retrieved",dateText(r.retrieved)]])+
 (r.description?'<h3>Description</h3><p class="description">'+esc(r.description)+'</p>':'')+
 provenancePanel(r.provenance)+'<details id="raw-details"><summary>View source data</summary><pre id="raw-content"></pre></details><p class="fine provenance">'+external(r.origin,"Publisher download ↗")+'</p>');
 $("raw-details").ontoggle=async()=>{const pre=$("raw-content");if(!pre||!$("raw-details").open||pre.textContent)return;pre.textContent="Loading…";try{pre.textContent=JSON.stringify(await api("/api/notice/"+id+"/raw"),null,2)}catch(e){pre.textContent=e.message}};
 }catch(e){if(modal==="notice")$("dialog-body").textContent=e.message}
}
$("records").onclick=e=>{const tr=e.target.closest("tr[data-id]");if(tr)openNotice(tr.dataset.id)};
function isoShift(d,shift){const dt=new Date(d+"T12:00:00Z");dt.setUTCDate(dt.getUTCDate()+shift);return dt.toISOString().slice(0,10)}
function jobHtml(){
 if(!job)return "";const active=["Collecting","Pausing"].includes(job.status);
 return '<section class="job-block"><h3>Collection · '+esc(job.status)+'</h3><p>'+dateText(job.start)+' – '+dateText(job.end)+'</p><ul class="job-list">'+job.tasks.map(t=>'<li><strong>'+esc(stats.sources[t.source].short)+'</strong><span>'+esc(t.status)+'</span>'+(t.message?'<small>'+esc(t.message)+'</small>':'')+'</li>').join("")+'</ul>'+
 (active?'<button class="btn secondary" data-action="pause" '+(job.status==="Pausing"?"disabled":"")+'>Pause collection</button>':["Paused","Needs attention"].includes(job.status)?'<button class="btn secondary" data-action="resume">Resume collection</button>':'')+'</section>'
}
function collection(){
 if(!stats)return;const end=isoShift(stats.today,-1);
 dialog("collect","Collection manager",'<h2>Collect notices</h2><p class="dialog-lead">Choose the official sources and dates to add to your local index.</p><div id="job-status">'+jobHtml()+'</div><form id="collect-form"><h3>Date range</h3><div class="form-row"><label>From<input required type="date" id="start-day" max="'+stats.today+'" value="'+end+'"></label><label>Through<input required type="date" id="end-day" max="'+stats.today+'" value="'+end+'"></label></div><div class="presets"><button type="button" data-days="1">Yesterday</button><button type="button" data-days="7">Last 7 days</button><button type="button" data-days="30">Last 30 days</button><button type="button" data-days="365">Last year</button></div><div class="checks-top"><h3>Sources</h3><button type="button" class="link" id="select-all">Deselect all</button></div><div class="check-list">'+Object.entries(stats.sources).filter(([k,s])=>k!=="portal" && !s.excluded).map(([k,s])=>'<label><input type="checkbox" name="sources" value="'+k+'" checked><strong>'+esc(s.short)+'</strong><small>'+esc(s.region)+'</small></label>').join("")+'</div><div class="dialog-actions"><button id="start-job" class="btn primary" type="submit">'+icon("download")+'Start collection</button></div><p class="error" id="form-error" role="alert"></p><p class="fine">Large historical ranges can take time and disk space. You can pause and resume a collection. Atlas waits at least one second between publisher requests and excludes sites that present a CAPTCHA.</p></form>');
 updateCollection();
 $("select-all").onclick=()=>{const boxes=[...document.querySelectorAll('[name="sources"]')];const checked=!boxes.every(b=>b.checked);for(const b of boxes)b.checked=checked;$("select-all").textContent=checked?"Deselect all":"Select all"};
 const syncAll=()=>{$("select-all").textContent=[...document.querySelectorAll('[name="sources"]')].every(b=>b.checked)?"Deselect all":"Select all"};
 for(const b of document.querySelectorAll('[name="sources"]'))b.onchange=syncAll;
 for(const b of document.querySelectorAll("[data-days]"))b.onclick=()=>{$("start-day").value=isoShift(end,1-Number(b.dataset.days));$("end-day").value=end};
 $("collect-form").onsubmit=async e=>{e.preventDefault();$("start-job").disabled=true;$("form-error").textContent="";
 try{job=await api("/api/collect",{start:$("start-day").value,end:$("end-day").value,sources:[...document.querySelectorAll('[name="sources"]:checked')].map(b=>b.value)});updateCollection();$("drawer").scrollTop=0}
 catch(err){$("form-error").textContent=err.message;$("start-job").disabled=false}};
}
function updateCollection(){if(modal!=="collect")return;$("job-status").innerHTML=jobHtml();$("start-job").disabled=!!job&&["Collecting","Pausing"].includes(job.status)}
document.querySelector('[data-act="collect"]').onclick=async()=>{try{job=await api("/api/status");collection()}catch(e){toast(e.message)}};
$("dialog-body").onclick=async e=>{const b=e.target.closest("[data-action]");if(!b)return;b.disabled=true;try{job=await api("/api/"+b.dataset.action,{});updateCollection()}catch(err){toast(err.message);b.disabled=false}};
$("quit").onclick=()=>{dialog("quit","Local app","<h2>Quit Tender Atlas?</h2><p class='dialog-lead'>Your index stays on this computer. You can resume an interrupted collection the next time you open the app.</p><button class='btn primary' id='quit-confirm'>Quit app</button>");$("quit-confirm").onclick=async()=>{await api("/api/quit",{});dialog("closed","Local app","<h2>Tender Atlas is closed</h2><p class='dialog-lead'>You can close this tab.</p>")}};
async function poll(){
 if(modal==="closed")return;
 try{const current=await api("/api/status");const sig=JSON.stringify(current);
 if(sig!==lastRunSignature){const previous=job;job=current;lastRunSignature=sig;updateCollection();
 if(current){document.querySelector(".sync span").textContent=["Collecting","Pausing"].includes(current.status)?"View collection":"Collect notices";
 if(previous&&previous.status!==current.status&&["Complete","Paused","Needs attention"].includes(current.status)){await readStats();await results();toast("Collection · "+current.status)}}}
 }catch{}finally{setTimeout(poll,3500)}
}
(async()=>{try{await readStats();setView(["#sources","#discover"].includes(location.hash)?location.hash.slice(1):"explorer");await results();poll()}catch(e){$("result-info").textContent=e.message}})();
