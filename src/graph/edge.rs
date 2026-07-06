use super::node::NodeId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transition {
    Goto(NodeId),
    End,
}
