//! Passes connections on from a port of archipel's pool to a game's own port on
//! this computer, for a game that will not host on another (the desktop app's
//! openings): the pool's ports are the ones the firewall here lets in and the
//! check from outside tries, so the game's own port needs neither. Connections
//! from this computer to itself do not go through the firewall.
//!
//! TCP only, as the games that need it use (Nebula's WebSockets). The game sees
//! every player come from this computer.

use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};

use anyhow::Context;
use tokio::net::{TcpListener, TcpStream};
use tracing::debug;

/// Relaying while it lives: dropping it stops listening and ends the
/// connections it passed on.
pub struct Relay {
    task: tokio::task::JoinHandle<()>,
    port: u16,
}

impl Relay {
    /// Listens on `port` (0: any, for tests), every address, and passes each
    /// connection on to `target` on this computer.
    pub async fn start(port: u16, target: u16) -> anyhow::Result<Relay> {
        let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, port))
            .await
            .with_context(|| format!("port {port} is in use"))?;
        let port = listener.local_addr()?.port();
        let task = tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => match accepted {
                        Ok((player, from)) => {
                            connections.spawn(pass_on(player, from, target));
                        }
                        Err(e) => debug!("relay on {port}: accepting: {e}"),
                    },
                    // Reaps finished connections, so the set does not grow.
                    Some(_) = connections.join_next(), if !connections.is_empty() => {}
                }
            }
        });
        Ok(Relay { task, port })
    }

    /// The port it listens on.
    pub fn port(&self) -> u16 {
        self.port
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        // Its connections are in the task's set, aborted with it.
        self.task.abort();
    }
}

/// Copies both ways between a player and the game, until either side closes.
async fn pass_on(mut player: TcpStream, from: SocketAddr, target: u16) {
    // A game listens on IPv4, IPv6 or both.
    let game = match TcpStream::connect((Ipv4Addr::LOCALHOST, target)).await {
        Ok(game) => Ok(game),
        Err(_) => TcpStream::connect((Ipv6Addr::LOCALHOST, target)).await,
    };
    let mut game = match game {
        Ok(game) => game,
        Err(e) => {
            debug!("relay from {from}: nothing listens on {target} ({e}): is the game hosting?");
            return;
        }
    };
    let _ = player.set_nodelay(true);
    let _ = game.set_nodelay(true);
    if let Err(e) = tokio::io::copy_bidirectional(&mut player, &mut game).await {
        debug!("relay from {from} to {target}: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn passes_bytes_both_ways() {
        // The game: answers what it is sent, upper-cased.
        let game = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let target = game.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut s, _) = game.accept().await.unwrap();
            let mut buf = [0u8; 5];
            s.read_exact(&mut buf).await.unwrap();
            s.write_all(&buf.to_ascii_uppercase()).await.unwrap();
        });
        let relay = Relay::start(0, target).await.unwrap();
        let mut player = TcpStream::connect((Ipv4Addr::LOCALHOST, relay.port()))
            .await
            .unwrap();
        player.write_all(b"hello").await.unwrap();
        let mut answer = [0u8; 5];
        player.read_exact(&mut answer).await.unwrap();
        assert_eq!(&answer, b"HELLO");
        // Stopped, it no longer listens.
        let port = relay.port();
        drop(relay);
        tokio::task::yield_now().await;
        assert!(
            TcpListener::bind((Ipv4Addr::UNSPECIFIED, port))
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn says_when_its_port_is_taken() {
        let taken = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)).await.unwrap();
        let port = taken.local_addr().unwrap().port();
        assert!(Relay::start(port, 1).await.is_err());
    }
}
