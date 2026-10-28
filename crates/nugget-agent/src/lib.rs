//! Nugget's own agent loop for computer use and multi-step tasks (M4).
//!
//! Work package M4-A builds: a tool registry, the plan/act/observe/verify loop with step and time
//! budgets, idempotent action ids, structured verification after every mutating action, policy
//! checks through `nugget_policy::PolicyGate` before every action, cancellation via the kill
//! switch, and a full action log for audit and replay. The computer-use driver plugs in as tools.
