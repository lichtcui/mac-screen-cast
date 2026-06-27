use std::process::Command;

/// List visible windows via Swift.
pub fn list_windows() -> Vec<(u32, String, String)> {
    let script = r#"
import Foundation
import CoreGraphics
let w = CGWindowListCopyWindowInfo(.optionAll, kCGNullWindowID) as! [[String:Any]]
for x in w { if let n = x[kCGWindowName as String] as? String, !n.isEmpty,
                let o = x[kCGWindowOwnerName as String] as? String,
                let l = x[kCGWindowLayer as String] as? NSNumber, l.intValue == 0 {
                  print("\(x[kCGWindowNumber as String] as! NSNumber) ||| \(o) ||| \(n)") } }
"#;
    let out = match Command::new("swift").arg("-e").arg(script).output() {
        Ok(o) => o,
        Err(_) => return Vec::new(),
    };
    if !out.status.success() { return Vec::new(); }
    String::from_utf8_lossy(&out.stdout).lines().filter_map(|l| {
        let p: Vec<&str> = l.trim().split(" ||| ").collect();
        if p.len() >= 3 { Some((p[0].parse().ok()?, p[1].into(), p[2].into())) } else { None }
    }).collect()
}

/// HTML page for the H.264 MSE video stream.
pub fn html() -> String {
    r#"<!DOCTYPE html><html><meta charset="utf-8"><meta name=viewport content="width=device-width,initial-scale=1,maximum-scale=1,user-scalable=no"><title>ScreenStream</title><style>*{margin:0;background:#000}body{display:flex;min-height:100vh;min-height:100dvh;align-items:center;justify-content:center}video{width:100%;max-height:100vh;max-height:100dvh}#b{position:fixed;bottom:0;left:0;right:0;display:flex;gap:12px;padding:3px 10px;background:rgba(0,0,0,.5);color:#aaa;font:11px/1.3 monospace;z-index:99;user-select:none}}.g{color:#4a4}.r{color:#c44}</style><body><video id=v autoplay muted playsinline></video><div id=b><span id=st class=g>loading</span></div><script>
let v=document.getElementById('v'),st=document.getElementById('st'),init=!1,m,ab,seg=0;
fetch('/init.mp4').then(r=>r.arrayBuffer()).then(b=>{
let d=new Uint8Array(b),p=-1;for(let i=0;i<d.length-4;i++){if(d[i]===0x61&&d[i+1]===0x76&&d[i+2]===0x63&&d[i+3]===0x43){p=i+5;break}}
let codecs='avc1.'+[d[p],d[p+1],d[p+2]].map(x=>x.toString(16).padStart(2,'0')).join('');
m=new MediaSource();m.onsourceopen=()=>{ab=m.addSourceBuffer('video/mp4;codecs="'+codecs+'"');ab.mode='sequence';ab.appendBuffer(b)};
v.src=URL.createObjectURL(m)
}).catch(()=>{st.textContent='no h264';st.className='r'});
(function p(){fetch('/seg?'+seg+'&'+Date.now()).then(r=>{
if(!r.ok){setTimeout(p,500);return}
seg=+r.headers.get('X-Seg')+1;return r.arrayBuffer()
}).then(b=>{if(b&&ab&&!ab.updating)try{ab.appendBuffer(b);st.textContent='h264';st.className='g'}catch(e){}setTimeout(p,30)}).catch(()=>setTimeout(p,1000))})()
</script>"#.into()
}

/// Get local IP address.
pub fn get_ip() -> String {
    Command::new("sh").arg("-c").arg("ipconfig getifaddr en0 2>/dev/null || echo 127.0.0.1")
        .output().map(|o| String::from_utf8_lossy(&o.stdout).trim().into()).unwrap_or_default()
}

