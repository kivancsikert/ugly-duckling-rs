use serde::de::DeserializeOwned;
use std::{collections::HashMap, sync::Arc};

trait CommandHandler {
    fn execute(&self, payload: &str);
}

struct Command<T> {
    delegate: Arc<dyn Fn(T) + Send + Sync>,
}

impl<T> CommandHandler for Command<T>
where
    T: DeserializeOwned + 'static,
{
    fn execute(&self, payload: &str) {
        match serde_json::from_str::<T>(payload) {
            Ok(data) => {
                (self.delegate)(data);
            }
            Err(e) => {
                eprintln!("Failed to deserialize payload: {}", e);
            }
        }
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

    pub fn register<T, F>(&mut self, path: &str, command: F)
    where
        T: DeserializeOwned + 'static,
        F: Fn(T) + Send + Sync + 'static,
    {
        self.commands.insert(
            path.to_string(),
            Box::new(Command {
                delegate: Arc::new(command),
            }),
        );
    }

    pub fn handle(&self, command: &str, payload: &str) {
        if let Some(command) = self.commands.get(command) {
            command.execute(payload);
        } else {
            eprintln!("Unregistered registered: {}", command);
        }
    }
}
