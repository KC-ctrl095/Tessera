//! User-only context, selection and find. No host clipboard is read, and no
//! page code is evaluated in the site's own world.
use serde_json::json;
use tessera_proto::{BrowserPageAction, BrowserPageResult};

use super::Tab;

pub(crate) const PDF_LIMIT: usize = 16 << 20;
pub(crate) const PDF_CHUNK: usize = 64 << 10;

// Close even on error, task cancellation or a stopped browser.
struct PdfStream {
    cdp: std::sync::Arc<super::cdp::Cdp>,
    session: String,
    handle: String,
}
impl Drop for PdfStream {
    fn drop(&mut self) {
        self.cdp.send(
            Some(&self.session),
            "IO.close",
            json!({"handle":self.handle}),
        );
    }
}

fn pdf_chunk(chunk: &serde_json::Value, remaining: usize) -> Result<Vec<u8>, String> {
    use base64::Engine as _;
    let data = chunk["data"].as_str().ok_or("the PDF couldn't be read")?;
    let bytes = if chunk["base64Encoded"] == true {
        if data.len() > remaining.div_ceil(3) * 4 {
            return Err("this PDF is larger than 16 MB; print it in your browser".into());
        }
        base64::engine::general_purpose::STANDARD
            .decode(data)
            .map_err(|_| "the PDF couldn't be read")?
    } else {
        if data.len() > remaining {
            return Err("this PDF is larger than 16 MB; print it in your browser".into());
        }
        data.as_bytes().to_vec()
    };
    if bytes.len() > remaining {
        return Err("this PDF is larger than 16 MB; print it in your browser".into());
    }
    Ok(bytes)
}

const SELECTION: &str = r#"let unsupported = false;
const frameDoc = (el) => { try { const doc = el.contentDocument; if (!doc) unsupported=true; return doc; } catch { unsupported=true; return null; } };
const selected = (doc) => {
  if (!doc) return '';
  let el = doc.activeElement, shadow;
  while (el?.shadowRoot?.activeElement) { shadow=el.shadowRoot; el=shadow.activeElement; }
  if (el?.tagName === 'INPUT' && el.type === 'password') return '';
  if (el && /^(INPUT|TEXTAREA)$/.test(el.tagName) && typeof el.selectionStart === 'number')
    return el.value.slice(el.selectionStart, el.selectionEnd);
  if (el?.tagName === 'IFRAME') return selected(frameDoc(el));
  return String(shadow?.getSelection?.() || doc.getSelection() || '');
};"#;

impl Tab {
    pub(crate) async fn page_action(
        &self,
        action: BrowserPageAction,
    ) -> Result<BrowserPageResult, String> {
        let link = self.link()?;
        let expression = match action {
            BrowserPageAction::Selection => format!(
                "(() => {{ {SELECTION} return {{text:selected(document).slice(0,65536),get unsupported(){{return unsupported}}}}; }})()"
            ),
            BrowserPageAction::Context { x, y, select_word } => {
                if !x.is_finite() || !y.is_finite() || x < 0.0 || y < 0.0 {
                    return Err("choose a point on the page".into());
                }
                format!(
                    r#"(() => {{ {SELECTION}
 const descend=(doc,x,y,depth=0) => {{
   let el=doc.elementFromPoint(x,y),shadows=[];
   while(el?.shadowRoot && shadows.length<16) {{
     const child=el.shadowRoot.elementFromPoint(x,y);if(!child || child===el)break;
     shadows.push(el.shadowRoot);el=child;
   }}
   if(el?.tagName==='IFRAME' && depth<16) {{
     const child=frameDoc(el),r=el.getBoundingClientRect();
     if(child && r.width>0 && r.height>0) return descend(child,(x-r.left)*el.offsetWidth/r.width-el.clientLeft,(y-r.top)*el.offsetHeight/r.height-el.clientTop,depth+1);
   }}
   return {{doc,x,y,el,shadows}};
 }};
 const hit=descend(document,{x},{y}),{{doc,x,y,el,shadows}}=hit;
 if ({select_word} && !unsupported && el && !(el.tagName==='INPUT' && el.type==='password')) {{
   const pos=doc.caretPositionFromPoint?.(x,y,{{shadowRoots:shadows}});
   const range=pos?doc.createRange():doc.caretRangeFromPoint?.(x,y);
   if(pos)range.setStart(pos.offsetNode,pos.offset);
   if (range?.startContainer.nodeType===3) {{
     const node=range.startContainer,text=node.textContent,at=range.startOffset;
     let a=at,b=at; while(a>0 && /[\p{{L}}\p{{N}}_]/u.test(text[a-1]))a--;
     while(b<text.length && /[\p{{L}}\p{{N}}_]/u.test(text[b]))b++;
     if(b>a){{range.setStart(node,a);range.setEnd(node,b);const s=doc.getSelection();s.removeAllRanges();s.addRange(range);}}
   }}
 }}
 const href=el?.closest('a[href]')?.href;
 return {{text:selected(doc).slice(0,65536),link:href && /^https?:/i.test(href)?href:null,get unsupported(){{return unsupported}}}};
}})()"#
                )
            }
            BrowserPageAction::Find { text, backwards } => {
                if text.len() > 512 {
                    return Err("find text must be shorter than 512 bytes".into());
                }
                format!(
                    "(() => {{ const found=window.find({},false,{},true,false,true,false);return {{text:'',found}}; }})()",
                    json!(text),
                    backwards
                )
            }
            BrowserPageAction::PrintPdf => {
                // A cancelled requester still needs to close the handle when
                // generation answers. The small completion task owns that lease;
                // a failed oneshot send drops the stream immediately.
                let (send, receive) = tokio::sync::oneshot::channel();
                let cdp = link.cdp.clone();
                let session = link.session.clone();
                tokio::spawn(async move {
                    let result = cdp.call(Some(&session), "Page.printToPDF", json!({"printBackground":true,"preferCSSPageSize":true,"transferMode":"ReturnAsStream"})).await
                        .map_err(|_| "this page can't print here; open it in your browser to print".to_string())
                        .and_then(|value| {
                            let handle = value["stream"].as_str().ok_or("the PDF couldn't be read")?.to_string();
                            Ok(PdfStream { cdp, session, handle })
                        });
                    let _ = send.send(result);
                });
                let stream = receive.await.map_err(|_| "the browser stopped")??;
                let mut bytes = Vec::new();
                loop {
                    let remaining = PDF_LIMIT.saturating_sub(bytes.len());
                    let chunk = link
                        .cdp
                        .call(
                            Some(&link.session),
                            "IO.read",
                            json!({"handle":stream.handle,"size":PDF_CHUNK.min(remaining + 1)}),
                        )
                        .await?;
                    let data = pdf_chunk(&chunk, remaining.min(PDF_CHUNK))?;
                    if data.is_empty() && chunk["eof"] != true {
                        return Err("the PDF stream stopped".into());
                    }
                    bytes.extend(data);
                    if chunk["eof"] == true {
                        break;
                    }
                }
                drop(stream);
                if !bytes.starts_with(b"%PDF-") {
                    return Err("the PDF couldn't be saved".into());
                }
                let browser = self.browser().ok_or("the browser stopped")?;
                super::private_dir(browser.downloads_dir()).map_err(|_| "can't save the PDF")?;
                let name = format!(
                    "{}-{}.pdf",
                    super::safe_name(&self.title()),
                    crate::util::random_token(8)
                );
                let path = browser.downloads_dir().join(name);
                let mut options = std::fs::OpenOptions::new();
                options.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.mode(0o600);
                }
                use std::io::Write;
                if options
                    .open(&path)
                    .and_then(|mut f| f.write_all(&bytes))
                    .is_err()
                {
                    let _ = std::fs::remove_file(&path);
                    return Err("can't save the PDF".into());
                }
                let offered = browser.host()?.downloaded(&super::Downloaded {
                    path: path.clone(),
                    board: self.board(),
                    session: self.chat(),
                });
                if !offered {
                    let _ = std::fs::remove_file(&path);
                    return Err("the PDF couldn't be offered for download".into());
                }
                return Ok(BrowserPageResult {
                    text: "PDF ready in the chat's downloads.".into(),
                    link: None,
                    found: None,
                });
            }
            BrowserPageAction::ClearFind => {
                "(() => {getSelection()?.removeAllRanges();return {text:''}})()".into()
            }
        };
        let value = self.eval_in(&link, &expression, false).await?;
        if value["unsupported"] == true {
            return Err("Copy and link actions in a cross-origin frame aren't available here. Open the page in your browser for those actions.".into());
        }
        serde_json::from_value(value).map_err(|_| "the page could not answer".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pdf_reads_reject_oversize_before_accumulating() {
        assert_eq!(
            pdf_chunk(&json!({"data":"%PDF-","eof":true}), 5).unwrap(),
            b"%PDF-"
        );
        assert!(pdf_chunk(&json!({"data":"123456"}), 5).is_err());
        assert!(pdf_chunk(&json!({"data":"MTIzNDU2","base64Encoded":true}), 5).is_err());
        assert!(pdf_chunk(&json!({"data":"eA==","base64Encoded":true}), 0).is_err());
        assert_eq!(pdf_chunk(&json!({"data":"","eof":true}), 0).unwrap(), b"");
    }
}
