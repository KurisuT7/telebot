use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use grammers_client::Client;
use grammers_client::message::Message;

use crate::command::Command;

#[derive(Clone)]
pub struct CommandContext {
    pub client: Client,
    pub message: Message,
    pub command: Command,
}

#[async_trait]
pub trait Plugin: Send + Sync {
    fn name(&self) -> &'static str;
    fn commands(&self) -> &'static [&'static str];
    async fn handle(&self, context: CommandContext) -> Result<()>;
}

#[derive(Default)]
pub struct Router {
    commands: HashMap<&'static str, Arc<dyn Plugin>>,
}

impl Router {
    pub fn register(&mut self, plugin: Arc<dyn Plugin>) -> Result<()> {
        for &command in plugin.commands() {
            if let Some(existing) = self.commands.insert(command, Arc::clone(&plugin)) {
                return Err(anyhow!(
                    "command {command} is registered by both {} and {}",
                    existing.name(),
                    plugin.name()
                ));
            }
        }
        Ok(())
    }

    pub fn plugin_for(&self, command: &str) -> Option<Arc<dyn Plugin>> {
        self.commands.get(command).cloned()
    }

    pub fn registered_commands(&self) -> Vec<&'static str> {
        let mut commands = self.commands.keys().copied().collect::<Vec<_>>();
        commands.sort_unstable();
        commands
    }
}
