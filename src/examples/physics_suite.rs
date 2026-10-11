//! Standard physics quality scenes, headless on the CPU solvers.
//!
//! ```sh
//! cargo run --release --example physics_suite
//! ```
//!
//! Prints one line per scene: its size, the mean step time, and the error a
//! correct solver keeps near zero (see `SuiteScene`).

use rusting_engine::runtime::SuiteScene;

fn main() {
    for (scene, size, steps) in [
        (SuiteScene::StackingStability, 20, 600),
        (SuiteScene::JointChain, 20, 600),
        (SuiteScene::CcdBullets, 400, 60),
        (SuiteScene::ClothHang, 40, 300),
        (SuiteScene::FluidDrop, 1000, 300),
        (SuiteScene::VehicleStress, 25, 600),
        (SuiteScene::FractureDrop, 50, 300),
        (SuiteScene::RagdollPile, 100, 300),
    ] {
        let score = scene.run(size, steps);
        println!(
            "{:<20} size {:>4}  step {:>9.3?}  {} {:.4}",
            scene.name(),
            size,
            score.mean_step,
            scene.error_unit(),
            score.error
        );
    }
}
