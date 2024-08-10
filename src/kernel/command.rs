use anyhow::{anyhow, Result};
use serde::{de::DeserializeOwned, Serialize};
use std::{collections::HashMap, sync::Arc};

trait CommandHandler {
    fn execute(&self, payload: &str) -> Result<Option<String>>;
}

struct Command<I, O> {
    delegate: Arc<dyn Fn(I) -> Result<Option<O>> + Send + Sync>,
}

impl<I, O> CommandHandler for Command<I, O>
where
    I: DeserializeOwned + 'static,
    O: Serialize + 'static,
{
    fn execute(&self, payload: &str) -> Result<Option<String>> {
        let data = serde_json::from_str(payload)?;
        let result = (self.delegate)(data)?;
        Ok(result
            .map(|result| serde_json::to_string(&result))
            .transpose()?)
    }
}

pub struct CommandManager {
    commands: HashMap<String, Box<dyn CommandHandler + Send + Sync>>,
}

impl CommandManager {
    pub fn new() -> Self {
        CommandManager {
            commands: HashMap::new(),
        }
    }

    pub fn register<I, O, F>(&mut self, path: &str, command: F)
    where
        I: DeserializeOwned + 'static,
        O: Serialize + 'static,
        F: Fn(I) -> Result<Option<O>> + Send + Sync + 'static,
    {
        self.commands.insert(
            path.to_string(),
            Box::new(Command {
                delegate: Arc::new(command),
            }),
        );
    }

    pub fn handle(&self, command_name: &str, payload: &str) -> Result<Option<String>> {
        if let Some(command) = self.commands.get(command_name) {
            let response = command.execute(payload)?;
            Ok(response)
        } else {
            Err(anyhow!("Unregistered command: {}", command_name))
        }
    }
}
