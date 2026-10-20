//! The relay's stdout is line buffered, but CDP messages end in NUL. Flush
//! each read, not just on EOF, so a small first reply cannot get stuck.
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub(super) async fn copy(
    mut reader: impl AsyncRead + Unpin,
    mut writer: impl AsyncWrite + Unpin,
) -> std::io::Result<()> {
    let mut bytes = vec![0; 64 * 1024];
    loop {
        let n = reader.read(&mut bytes).await?;
        if n == 0 {
            return Ok(());
        }
        writer.write_all(&bytes[..n]).await?;
        writer.flush().await?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn small_nul_terminated_reply_arrives_before_eof() {
        let (mut source, read) = tokio::io::duplex(1024);
        let (write, mut sink) = tokio::io::duplex(1024);
        let task = tokio::spawn(copy(read, tokio::io::BufWriter::new(write)));
        let reply = b"{\"id\":1}\0";
        source.write_all(reply).await.unwrap();
        let mut result = [0; 9];
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            sink.read_exact(&mut result),
        )
        .await
        .expect("reply was buffered until EOF")
        .unwrap();
        assert_eq!(&result, reply);
        drop(source);
        task.await.unwrap().unwrap();
    }
}
