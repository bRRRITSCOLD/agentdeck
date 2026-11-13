//! At most one read of the daemon in flight per view: a change that arrives while a read runs
//! only marks it stale, and one more read runs when it lands. A burst of changes, such as a PR
//! sync's, then costs two reads instead of one each.

/// Whether a view's read is running, and whether a change came in meanwhile.
#[derive(Debug, Default)]
pub struct Rereads {
    running: bool,
    stale: bool,
}

impl Rereads {
    /// A read is wanted: whether to start one now. While one runs, it is marked stale instead.
    pub fn ask(&mut self) -> bool {
        if self.running {
            self.stale = true;
            false
        } else {
            self.running = true;
            true
        }
    }

    /// The running read landed: whether to read again for a change that came meanwhile.
    pub fn landed(&mut self) -> bool {
        self.running = false;
        std::mem::take(&mut self.stale)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_start_a_read_when_none_runs() {
        assert!(Rereads::default().ask());
    }

    #[test]
    fn should_not_start_another_while_one_runs() {
        let mut rereads = Rereads::default();
        rereads.ask();

        assert!(!rereads.ask());
        assert!(!rereads.ask());
    }

    #[test]
    fn should_read_once_more_when_asked_while_running() {
        let mut rereads = Rereads::default();
        rereads.ask();
        rereads.ask();
        rereads.ask();

        assert!(rereads.landed());
    }

    #[test]
    fn should_not_read_again_when_nothing_came_meanwhile() {
        let mut rereads = Rereads::default();
        rereads.ask();

        assert!(!rereads.landed());
        assert!(rereads.ask());
    }
}
