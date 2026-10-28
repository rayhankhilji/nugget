//! [`ComputerUse`](super::ComputerUse) for [`Backend`](crate::Backend): policy, id resolution and
//! read-back verification over any [`Platform`](crate::Platform).

use nugget_core::Result;

use super::{
    ActionOutcome, AppEntry, ComputerUse, ElementId, KeyInput, TreeLimits, UiAction, UiTree,
    WindowInfo, WindowSel,
};
use crate::Permission;
use crate::engine::{Backend, Platform};

impl<P: Platform> ComputerUse for Backend<P> {
    fn permission(&self) -> Permission {
        self.platform().permission()
    }
    fn apps(&self) -> Result<Vec<AppEntry>> {
        todo!("W2-A")
    }
    fn windows(&self) -> Result<Vec<WindowInfo>> {
        todo!("W2-A")
    }
    fn snapshot(&self, _window: &WindowSel, _limits: &TreeLimits) -> Result<UiTree> {
        todo!("W2-A")
    }
    fn act(&self, _id: &ElementId, _action: &UiAction) -> Result<ActionOutcome> {
        todo!("W2-A")
    }
    fn key_input(&self, _t: Option<&ElementId>, _i: &KeyInput) -> Result<ActionOutcome> {
        todo!("W2-A")
    }
    fn open_app(&self, _name: &str) -> Result<ActionOutcome> {
        todo!("W2-A")
    }
}
