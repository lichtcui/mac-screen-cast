use std::process::Command;

/// A visible window.
#[derive(serde::Serialize)]
pub struct Window {
    pub id: u32,
    pub app: String,
    pub title: String,
}

/// List visible windows via Swift.
pub fn list_windows() -> Vec<Window> {
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
    if !out.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let p: Vec<&str> = l.trim().split(" ||| ").collect();
            if p.len() >= 3 {
                Some(Window {
                    id: p[0].parse().ok()?,
                    app: p[1].into(),
                    title: p[2].into(),
                })
            } else {
                None
            }
        })
        .collect()
}

/// List windows as JSON array.
pub fn list_windows_json() -> String {
    serde_json::to_string(&list_windows()).unwrap_or_else(|_| "[]".into())
}

/// WebRTC video page with real-time latency display.
pub fn html(_fps: u32) -> String {
    r#"<!DOCTYPE html><html><meta charset="utf-8"><meta name=viewport content="width=device-width,initial-scale=1,maximum-scale=1,user-scalable=no"><title>ScreenStream</title><style>*{margin:0;background:#000}body{display:flex;min-height:100vh;min-height:100dvh;align-items:center;justify-content:center}video{width:100%;max-height:100vh;max-height:100dvh}#b{position:fixed;bottom:0;left:0;right:0;display:flex;gap:12px;padding:3px 10px;background:rgba(0,0,0,.5);color:#aaa;font:11px/1.3 monospace;z-index:99;user-select:none}}.g{color:#4a4}.r{color:#c44}</style><body><video id=v autoplay muted playsinline></video><div id=b><span id=st class=r>connecting</span></div><script>
var v=document.getElementById('v'),st=document.getElementById('st'),pc;
fetch('/offer').then(r=>r.text()).then(async o=>{
pc=new RTCPeerConnection({iceServers:[{urls:'stun:stun.l.google.com:19302'}]});
pc.ontrack=e=>{v.srcObject=e.streams[0];v.onloadedmetadata=()=>st.className='g'};
pc.oniceconnectionstatechange=()=>{var s=pc.iceConnectionState;st.textContent=s;if(s==='failed')console.log('ICE failed')};
pc.onicecandidateerror=e=>console.warn('ICE candidate error:',e.errorText||'timeout',e.url||'');
setInterval(async()=>{
  try{
    var lat=await(await fetch('/latency')).text();
    st.textContent=lat+'ms latency'
  }catch(e){}
},1000);
var candidates=[];
pc.onicecandidate=e=>{if(e.candidate)candidates.push({candidate:e.candidate.candidate,sdpMid:e.candidate.sdpMid,sdpMLineIndex:e.candidate.sdpMLineIndex})};
pc.addTransceiver('video',{direction:'recvonly'});
await pc.setRemoteDescription({type:'offer',sdp:o});
var a=await pc.createAnswer();
await pc.setLocalDescription(a);
await new Promise(r=>{if(pc.iceGatheringState==='complete')r();else pc.onicegatheringstatechange=ev=>{if(pc.iceGatheringState==='complete')r()}});
var msg={sdp:a.sdp,candidates:candidates};
fetch('/signal',{method:'POST',body:JSON.stringify(msg)})
}).catch(e=>{st.textContent='error: '+e.message;st.className='r'})
</script>"#.to_string()
}

/// Get local IP address.
pub fn get_ip() -> String {
    Command::new("sh")
        .arg("-c")
        .arg("ipconfig getifaddr en0 2>/dev/null || echo 127.0.0.1")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().into())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_contains_video_tag() {
        let page = html(30);
        assert!(page.contains("<video"));
        assert!(page.contains("/offer"));
        assert!(page.contains("/latency"));
        assert!(page.contains("/signal"));
    }

    #[test]
    fn html_contains_stun() {
        let page = html(30);
        assert!(page.contains("stun:stun.l.google.com:19302"));
    }

    #[test]
    fn get_ip_returns_string() {
        let ip = get_ip();
        assert!(!ip.is_empty());
        assert!(ip.contains('.'));
    }
}
