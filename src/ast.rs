use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
pub enum Ast {
    Print {
        message: String,
    },

    /// The prompt can include a message "invoke subroutine N"
    Think {
        think: Think,
    },

    Do {
        children: Vec<Ast>,
    },
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Think {
    pub prompt: String,
    pub children: Vec<Ast>,
}
