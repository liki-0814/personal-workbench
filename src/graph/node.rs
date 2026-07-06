#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NodeId {
    Model,
    Tool,
    UserApproval,
    End,
}
