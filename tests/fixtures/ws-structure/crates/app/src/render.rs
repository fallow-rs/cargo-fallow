use crate::engine::run;
use crate::engine::shared::helper;

pub fn draw() {
    run();
    super::engine::stop();
    helper();
}
