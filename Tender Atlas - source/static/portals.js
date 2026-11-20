/* Portal previews live alongside the searchable register. */
let portalPreview = null;
let portalBusy = false;

async function loadSavedPortals() {
  const urls = await api("/api/portals");
  $("portal-saved").innerHTML = urls.length ? '<label>Saved portals <select id="saved-portal"><option value="">Choose a portal</option>' +
    urls.map(url => '<option value="'+esc(url)+'">'+esc(url)+'</option>').join("") + '</select></label>' : "";
  if (urls.length) $("saved-portal").onchange = event => {
    if (!event.target.value || portalBusy) return;
    $("portal-url").value = event.target.value;
    runPortal(false);
  };
}

function showPortal(data) {
  const s = data.sample;
  $("portal-status").textContent = data.warning || (s ? "Read "+new Date(s.checked).toLocaleString() : "");
  const html = [];
  if (s) {
    const heading = s.latest_issue ? "Latest TED notices" : "Notice preview";
    html.push('<h3>'+heading+'</h3><ul class="portal-notices">'+s.rows.map(r => '<li>'+external(r.url,r.title)+
      '<div>'+[r.publication_number, r.buyer || "Buyer unavailable", r.published || "Date unavailable"].filter(Boolean).map(esc).join(" · ")+'</div></li>').join("")+'</ul>',
      '<h3>Source checks</h3><ul>'+s.checks.map(x=>'<li>'+esc(x)+'</li>').join("")+'</ul>');
    if(s.sitemap) html.push('<p>'+external(s.sitemap,"Source sitemap")+'</p>');
    if(s.latest_issue) html.push('<p>'+external(s.latest_issue,"Browse this issue on TED")+'</p>');
    if(s.verified) html.push('<button type="button" class="btn secondary" id="portal-add">Add sample to register</button>');
  }
  if (data.candidates.length) {
    html.push('<details><summary>Review candidate links</summary><p>Your labels train the ranking for subsequent discoveries.</p><ol>'+
      data.candidates.map((r,i)=>'<li>'+external(r.url,r.text || r.url)+' <button type="button" data-label="1" data-i="'+i+'">Notice source</button> <button type="button" data-label="0" data-i="'+i+'">Other page</button></li>').join("")+'</ol></details>');
  }
  $("portal-result").innerHTML=html.join("");
}

async function runPortal(force) {
  if(portalBusy || !$("portal-url").reportValidity()) return;
  portalBusy=true;
  $("portal-read").disabled=true;
  $("portal-status").textContent="Reading public pages and checking the sample…";
  $("portal-result").innerHTML="";
  portalPreview=null;
  try {
    portalPreview=await api("/api/portal-preview",{url:$("portal-url").value.trim(),refresh:force});
    showPortal(portalPreview);
    await loadSavedPortals();
  } catch(e) { $("portal-status").textContent=e.message; }
  finally { portalBusy=false; $("portal-read").disabled=false; }
}

$("portal-form").onsubmit=e=>{e.preventDefault();runPortal(true)};
$("portal-result").onclick=async e=>{
  const b=e.target.closest("button");
  if(!b || !portalPreview) return;
  b.disabled=true;
  try {
    if(b.id==="portal-add"){
      const res=await api("/api/portal-import",{entry:portalPreview.entry});
      await readStats(); await results();
      toast(res.count+" notices saved in the register.");
    } else if(b.dataset.label!==undefined){
      const item=portalPreview.candidates[Number(b.dataset.i)];
      await api("/api/portal-label",{entry:portalPreview.entry,url:item.url,label:Number(b.dataset.label)});
      for(const sibling of b.parentElement.querySelectorAll("button")) sibling.disabled=sibling===b;
      toast("Training label saved.");
    }
  } catch(err){b.disabled=false;toast(err.message)}
};
loadSavedPortals().catch(e=>{$("portal-status").textContent=e.message});
const autoRefresh = $("auto-refresh");
async function loadRefreshPreference(){
  try {
    const preference = await api("/settings/auto-refresh");
    autoRefresh.checked = preference.enabled;
    autoRefresh.disabled = false;
    return preference.enabled;
  } catch {
    autoRefresh.checked = false;
    return false;
  }
}
autoRefresh.onchange = async () => {
  const enabled = autoRefresh.checked;
  autoRefresh.disabled = true;
  try {
    await api("/settings/auto-refresh", {enabled});
    toast(enabled ? "Automatic portal refresh enabled." : "Automatic portal refresh off.");
  } catch (error) {
    await loadRefreshPreference();
    toast(error.message);
  } finally {
    autoRefresh.disabled = false;
  }
};
loadRefreshPreference();
window.addEventListener("focus", loadRefreshPreference);
setInterval(async ()=>{
  if(view==="discover" && autoRefresh.checked && await loadRefreshPreference() && !document.hidden && portalPreview && !portalPreview.excluded && !portalBusy &&
     $("portal-url").value.trim().replace(/\/$/,"")===portalPreview.entry.replace(/\/$/,"")) runPortal(false);
},300000);

