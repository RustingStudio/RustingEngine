//! Persistent types used by the Blender-style editor area layout.

use serde::{Deserialize, Serialize};

/// Content that can be placed in any editor area.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EditorPanel {
    /// 3D view that uses the editor camera.
    Scene,
    /// 3D view that uses the active game camera.
    Game,
    /// Text editor for Rust and shader files.
    Code,
    /// Tree of all objects in the scene.
    Hierarchy,
    /// Settings for the selected object.
    Inspector,
    /// Project paths, render settings, and asset counts.
    Project,
    /// Messages created by the editor and engine.
    Console,
    /// List of assets loaded by the engine.
    Assets,
    /// Rebinds editor keyboard shortcuts.
    Shortcuts,
    /// CPU and GPU frame timings, work counters, and memory history.
    Profiler,
    /// Scene render controls, device capabilities, and asset fallbacks.
    RenderSettings,
    /// Physics backend, workload, readback, and capacity diagnostics.
    PhysicsDiagnostics,
    /// Journal of scene changes written by tools outside the editor.
    Agent,
    /// Dope sheet for the selected object's animation clips.
    Timeline,
    /// Host or join a live editing session.
    Collaboration,
}

impl EditorPanel {
    pub(super) const ALL: [Self; 15] = [
        Self::Scene,
        Self::Game,
        Self::Code,
        Self::Hierarchy,
        Self::Inspector,
        Self::Project,
        Self::Console,
        Self::Assets,
        Self::Shortcuts,
        Self::Profiler,
        Self::RenderSettings,
        Self::PhysicsDiagnostics,
        Self::Agent,
        Self::Timeline,
        Self::Collaboration,
    ];

    pub(super) fn icon(self) -> super::EditorIcon {
        use super::EditorIcon;
        match self {
            Self::Scene => EditorIcon::Mesh,
            Self::Game => EditorIcon::Play,
            Self::Code => EditorIcon::Code,
            Self::Hierarchy => EditorIcon::Tree,
            Self::Inspector => EditorIcon::ViewOptions,
            Self::Project => EditorIcon::Gear,
            Self::Console => EditorIcon::Console,
            Self::Assets => EditorIcon::Folder,
            Self::Shortcuts => EditorIcon::Gear,
            Self::Profiler => EditorIcon::ViewOptions,
            Self::RenderSettings => EditorIcon::Gear,
            Self::PhysicsDiagnostics => EditorIcon::ViewOptions,
            Self::Agent => EditorIcon::Console,
            Self::Timeline => EditorIcon::Play,
            Self::Collaboration => EditorIcon::Eye,
        }
    }

    pub(super) fn title(self) -> &'static str {
        match self {
            Self::Scene => "Scene View",
            Self::Game => "Game View",
            Self::Code => "Code Editor",
            Self::Hierarchy => "Hierarchy",
            Self::Inspector => "Inspector",
            Self::Project => "Project Settings",
            Self::Console => "Console",
            Self::Assets => "Assets",
            Self::Shortcuts => "Keyboard Shortcuts",
            Self::Profiler => "Profiler",
            Self::RenderSettings => "Render Settings",
            Self::PhysicsDiagnostics => "Physics Diagnostics",
            Self::Agent => "Agent",
            Self::Timeline => "Timeline",
            Self::Collaboration => "Collaboration",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EditorSplitAxis {
    /// Places two areas next to each other.
    Columns,
    /// Places one area above the other.
    Rows,
}

/// Persistent Blender-style area tree. A leaf selects its editor type; a split
/// owns two more areas and a draggable divider.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum EditorDockNode {
    /// One visible editor area.
    Area {
        /// Stable number used by buttons and egui controls.
        id: u64,
        /// Content currently shown inside this area.
        panel: EditorPanel,
    },
    /// Divider that owns two smaller layout nodes.
    Split {
        /// Controls whether children are side by side or top and bottom.
        axis: EditorSplitAxis,
        /// Part of the available size given to the first child.
        ratio: f32,
        /// Area or split shown before the divider.
        first: Box<Self>,
        /// Area or split shown after the divider.
        second: Box<Self>,
    },
}

#[derive(Serialize, Deserialize)]
pub(super) struct EditorLayoutFile {
    /// Complete area tree saved to disk.
    pub(super) layout: EditorDockNode,
    /// Area that had the blue selection border.
    pub(super) active_area: u64,
    /// Number that will be given to the next new area.
    pub(super) next_area_id: u64,
    /// Gizmo snap on/off and increments. Files saved before this field
    /// existed leave the current snap settings alone.
    #[serde(default)]
    pub(super) snap: Option<(bool, super::shortcuts::SnapSteps)>,
}

impl EditorDockNode {
    /// Creates the layout shown when the editor starts for the first time.
    pub(super) fn default_layout() -> Self {
        // First build the top part: hierarchy, scene, and inspector.
        let main = Self::Split {
            axis: EditorSplitAxis::Columns,
            ratio: 0.18,
            first: Box::new(Self::Area {
                id: 1,
                panel: EditorPanel::Hierarchy,
            }),
            second: Box::new(Self::Split {
                axis: EditorSplitAxis::Columns,
                ratio: 0.72,
                first: Box::new(Self::Area {
                    id: 2,
                    panel: EditorPanel::Scene,
                }),
                second: Box::new(Self::Area {
                    id: 3,
                    panel: EditorPanel::Inspector,
                }),
            }),
        };
        // Put project settings below the main part.
        Self::Split {
            axis: EditorSplitAxis::Rows,
            ratio: 0.82,
            first: Box::new(main),
            second: Box::new(Self::Area {
                id: 4,
                panel: EditorPanel::Project,
            }),
        }
    }

    /// Changes the content of the area with the given ID.
    ///
    /// Returns true when the area was found.
    pub(super) fn set_panel(&mut self, id: u64, panel: EditorPanel) -> bool {
        match self {
            Self::Area {
                id: area_id,
                panel: current,
            } if *area_id == id => {
                *current = panel;
                true
            }
            Self::Area { .. } => false,
            Self::Split { first, second, .. } => {
                first.set_panel(id, panel) || second.set_panel(id, panel)
            }
        }
    }

    /// Returns an area that still exists after another area is closed.
    pub(super) fn first_area_id(&self) -> u64 {
        match self {
            Self::Area { id, .. } => *id,
            Self::Split { first, .. } => first.first_area_id(),
        }
    }

    /// Replaces one area with a divider and two copies of that area.
    pub(super) fn split(
        &mut self,
        id: u64,
        axis: EditorSplitAxis,
        new_id: u64,
    ) -> bool {
        match self {
            Self::Area { id: area_id, panel } if *area_id == id => {
                let old_panel = *panel;
                *self = Self::Split {
                    axis,
                    ratio: 0.5,
                    first: Box::new(Self::Area {
                        id,
                        panel: old_panel,
                    }),
                    second: Box::new(Self::Area {
                        id: new_id,
                        panel: old_panel,
                    }),
                };
                true
            }
            Self::Area { .. } => false,
            Self::Split { first, second, .. } => {
                first.split(id, axis, new_id) || second.split(id, axis, new_id)
            }
        }
    }

    /// Removes one area and lets its neighbour use the free space.
    pub(super) fn close(&mut self, id: u64) -> bool {
        let Self::Split { first, second, .. } = self else {
            return false;
        };
        if matches!(first.as_ref(), Self::Area { id: area_id, .. } if *area_id == id)
        {
            *self = *second.clone();
            return true;
        }
        if matches!(second.as_ref(), Self::Area { id: area_id, .. } if *area_id == id)
        {
            *self = *first.clone();
            return true;
        }
        first.close(id) || second.close(id)
    }
}
