//! The declared pass schedule of one `SceneRenderer` frame.
//!
//! Each pass names the resources it reads and writes and the image layout it
//! needs them in. `SceneRenderer` records its passes in this order, labels
//! each one with [`FramePass::label`], and builds its render passes with the
//! attachment layouts this table asks for. [`FrameGraph`] schedules any
//! enabled subset of the passes: their order, the barriers between them,
//! and the lifetimes of transient resources. Vulkano still inserts the
//! barriers it records.

use vulkano::image::ImageLayout;

/// One pass of a `SceneRenderer` frame, in submission order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FramePass {
    /// Staged material texture copies and physics state carry-over after a
    /// body buffer grows. Only frames that have either.
    Uploads,
    /// Fixed-tick GPU physics dispatches. Only frames with new ticks.
    Physics,
    /// GPU frustum culling into the visible list and indirect draws. Only
    /// frames that select the GPU culling path.
    Culling,
    /// Depth-only directional shadow map.
    Shadow,
    /// Depth-only draw of the opaque scene that ambient occlusion traces.
    /// Only frames with ambient occlusion.
    DepthPrepass,
    /// Screen-space ambient occlusion traced from the prepass depth and
    /// blurred. Only frames with ambient occlusion.
    AmbientOcclusion,
    /// Lit scene into the HDR target (main render pass, subpass 0). On
    /// occlusion frames, only the opaque instances visible last frame, in
    /// an early render pass that keeps HDR and depth.
    Scene,
    /// Farthest-depth mip chain of the early scene depth. Occlusion frames
    /// only.
    DepthPyramid,
    /// Late cull against the depth pyramid into a second visible list and
    /// draw-command set. Occlusion frames only.
    OcclusionCulling,
    /// The rest of the scene over the early pass (late render pass,
    /// subpass 0). Occlusion frames only.
    LateScene,
    /// Copy of the opaque HDR with its mip chain, and of the opaque depth
    /// into depth pyramid mip 0 for screen-space reflections. Frames with
    /// refracting or glossy materials only.
    SceneColor,
    /// Reflection overlay and blended draws after the copy (late render
    /// pass, subpass 0). Frames with refracting or glossy materials only;
    /// other frames draw blended objects in the scene pass.
    Transparent,
    /// Bright parts of the finished HDR blurred through a mip chain. Only
    /// frames with bloom.
    Bloom,
    /// Average brightness of the finished HDR, adapting the exposure. Only
    /// frames with auto exposure.
    Exposure,
    /// HDR target mapped into the output (main render pass, subpass 1).
    ToneMap,
    /// 2D sprites over the output in canvas order (main render pass,
    /// subpass 1). Only frames with sprites.
    Sprites,
    /// Editor debug lines over the output (main render pass, subpass 1).
    DebugOverlay,
}

/// A GPU resource that frame passes hand to each other.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FrameResource {
    MaterialTextures,
    PhysicsStates,
    /// Visible list and indirect draw commands written by GPU culling.
    DrawCommands,
    ShadowMap,
    DepthPyramid,
    HdrColor,
    /// Mipmapped copy of the opaque `HdrColor` that refraction samples.
    /// Ambient occlusion reads the previous frame's copy before it is
    /// replaced.
    SceneColor,
    SceneDepth,
    /// Blurred ambient occlusion per target pixel.
    AmbientOcclusion,
    /// Glow that tone mapping adds to the HDR color.
    Bloom,
    /// Adapted auto exposure factor, kept from frame to frame.
    Exposure,
    /// The caller's output image (swapchain image or offscreen target).
    Target,
}

/// How a pass uses one resource. `layout` is `None` for buffers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PassAccess {
    pub resource: FrameResource,
    pub layout: Option<ImageLayout>,
    pub write: bool,
}

/// A layout change of `resource` between two passes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LayoutTransition {
    pub resource: FrameResource,
    pub from: ImageLayout,
    pub to: ImageLayout,
    pub before: FramePass,
}

impl FramePass {
    pub const ALL: [FramePass; 17] = [
        FramePass::Uploads,
        FramePass::Physics,
        FramePass::Culling,
        FramePass::Shadow,
        FramePass::DepthPrepass,
        FramePass::AmbientOcclusion,
        FramePass::Scene,
        FramePass::DepthPyramid,
        FramePass::OcclusionCulling,
        FramePass::LateScene,
        FramePass::SceneColor,
        FramePass::Transparent,
        FramePass::Bloom,
        FramePass::Exposure,
        FramePass::ToneMap,
        FramePass::Sprites,
        FramePass::DebugOverlay,
    ];

    /// The debug-utils label the pass is recorded under.
    pub fn label(self) -> &'static str {
        match self {
            FramePass::Uploads => "Uploads",
            FramePass::Physics => "Physics",
            FramePass::Culling => "Culling",
            FramePass::Shadow => "Shadow",
            FramePass::DepthPrepass => "DepthPrepass",
            FramePass::AmbientOcclusion => "AmbientOcclusion",
            FramePass::Scene => "Scene",
            FramePass::DepthPyramid => "DepthPyramid",
            FramePass::OcclusionCulling => "OcclusionCulling",
            FramePass::LateScene => "LateScene",
            FramePass::SceneColor => "SceneColor",
            FramePass::Transparent => "Transparent",
            FramePass::Bloom => "Bloom",
            FramePass::Exposure => "Exposure",
            FramePass::ToneMap => "ToneMap",
            FramePass::Sprites => "Sprites",
            FramePass::DebugOverlay => "DebugOverlay",
        }
    }

    pub fn accesses(self) -> &'static [PassAccess] {
        use FrameResource::*;
        use ImageLayout::*;
        const fn read(
            resource: FrameResource,
            layout: Option<ImageLayout>,
        ) -> PassAccess {
            PassAccess {
                resource,
                layout,
                write: false,
            }
        }
        const fn write(
            resource: FrameResource,
            layout: Option<ImageLayout>,
        ) -> PassAccess {
            PassAccess {
                resource,
                layout,
                write: true,
            }
        }
        match self {
            FramePass::Uploads => {
                const {
                    &[
                        write(MaterialTextures, Some(TransferDstOptimal)),
                        write(PhysicsStates, None),
                    ]
                }
            }
            FramePass::Physics => {
                const { &[read(PhysicsStates, None), write(PhysicsStates, None)] }
            }
            FramePass::Culling => {
                const { &[read(PhysicsStates, None), write(DrawCommands, None)] }
            }
            FramePass::Shadow => {
                const { &[write(ShadowMap, Some(DepthStencilAttachmentOptimal))] }
            }
            FramePass::DepthPrepass => {
                const {
                    &[
                        read(MaterialTextures, Some(ShaderReadOnlyOptimal)),
                        read(PhysicsStates, None),
                        read(DrawCommands, None),
                        write(SceneDepth, Some(DepthStencilAttachmentOptimal)),
                    ]
                }
            }
            FramePass::AmbientOcclusion => {
                const {
                    &[
                        read(SceneDepth, Some(ShaderReadOnlyOptimal)),
                        // Last frame's copy, for indirect light.
                        read(SceneColor, Some(ShaderReadOnlyOptimal)),
                        write(AmbientOcclusion, Some(General)),
                    ]
                }
            }
            FramePass::Scene | FramePass::LateScene => {
                const {
                    &[
                        read(MaterialTextures, Some(ShaderReadOnlyOptimal)),
                        read(PhysicsStates, None),
                        read(DrawCommands, None),
                        read(ShadowMap, Some(ShaderReadOnlyOptimal)),
                        read(AmbientOcclusion, Some(ShaderReadOnlyOptimal)),
                        write(HdrColor, Some(ColorAttachmentOptimal)),
                        write(SceneDepth, Some(DepthStencilAttachmentOptimal)),
                    ]
                }
            }
            FramePass::SceneColor => {
                const {
                    &[
                        read(HdrColor, Some(TransferSrcOptimal)),
                        read(SceneDepth, Some(ShaderReadOnlyOptimal)),
                        write(SceneColor, Some(TransferDstOptimal)),
                        write(DepthPyramid, Some(General)),
                    ]
                }
            }
            FramePass::Transparent => {
                const {
                    &[
                        read(MaterialTextures, Some(ShaderReadOnlyOptimal)),
                        read(PhysicsStates, None),
                        read(DrawCommands, None),
                        read(ShadowMap, Some(ShaderReadOnlyOptimal)),
                        read(AmbientOcclusion, Some(ShaderReadOnlyOptimal)),
                        read(SceneColor, Some(ShaderReadOnlyOptimal)),
                        read(DepthPyramid, Some(ShaderReadOnlyOptimal)),
                        write(HdrColor, Some(ColorAttachmentOptimal)),
                        write(SceneDepth, Some(DepthStencilAttachmentOptimal)),
                    ]
                }
            }
            FramePass::DepthPyramid => {
                const {
                    &[
                        read(SceneDepth, Some(ShaderReadOnlyOptimal)),
                        write(DepthPyramid, Some(General)),
                    ]
                }
            }
            FramePass::OcclusionCulling => {
                const {
                    &[
                        read(PhysicsStates, None),
                        read(DepthPyramid, Some(ShaderReadOnlyOptimal)),
                        write(DrawCommands, None),
                    ]
                }
            }
            FramePass::Bloom => {
                const {
                    &[
                        read(HdrColor, Some(ShaderReadOnlyOptimal)),
                        write(Bloom, Some(General)),
                    ]
                }
            }
            FramePass::Exposure => {
                const {
                    &[
                        read(HdrColor, Some(ShaderReadOnlyOptimal)),
                        write(Exposure, None),
                    ]
                }
            }
            FramePass::ToneMap => {
                const {
                    &[
                        read(HdrColor, Some(ShaderReadOnlyOptimal)),
                        read(Bloom, Some(ShaderReadOnlyOptimal)),
                        read(Exposure, None),
                        write(Target, Some(ColorAttachmentOptimal)),
                    ]
                }
            }
            FramePass::Sprites => {
                const {
                    &[
                        read(MaterialTextures, Some(ShaderReadOnlyOptimal)),
                        write(Target, Some(ColorAttachmentOptimal)),
                    ]
                }
            }
            FramePass::DebugOverlay => {
                const {
                    &[
                        read(SceneDepth, Some(DepthStencilAttachmentOptimal)),
                        write(Target, Some(ColorAttachmentOptimal)),
                    ]
                }
            }
        }
    }

    /// The layout the first pass after `self` that touches `resource`
    /// expects, which is the layout `self` must leave it in.
    pub fn next_layout(self, resource: FrameResource) -> Option<ImageLayout> {
        FramePass::ALL
            .iter()
            .skip_while(|pass| **pass != self)
            .skip(1)
            .flat_map(|pass| pass.accesses())
            .find(|access| access.resource == resource)
            .and_then(|access| access.layout)
    }
}

/// Every layout change between consecutive users of an image, in order.
pub fn layout_transitions() -> Vec<LayoutTransition> {
    let mut last = Vec::<(FrameResource, ImageLayout)>::new();
    let mut transitions = Vec::new();
    for pass in FramePass::ALL {
        for access in pass.accesses() {
            let Some(layout) = access.layout else {
                continue;
            };
            match last.iter_mut().find(|(r, _)| *r == access.resource) {
                Some((_, from)) if *from != layout => {
                    transitions.push(LayoutTransition {
                        resource: access.resource,
                        from: *from,
                        to: layout,
                        before: pass,
                    });
                    *from = layout;
                }
                Some(_) => {}
                None => last.push((access.resource, layout)),
            }
        }
    }
    transitions
}

impl FrameResource {
    /// Whether the resource outlives the frame: it comes from outside the
    /// passes, or a later frame reads it. The others are transient, and
    /// transients alive in disjoint passes may share memory.
    pub fn persistent(self) -> bool {
        use FrameResource::*;
        // Draw commands keep the visible set the next occlusion frame draws
        // first.
        matches!(
            self,
            MaterialTextures
                | PhysicsStates
                | DrawCommands
                | SceneColor
                | Exposure
                | Target
        )
    }
}

/// A dependency the frame graph places before `before` on one resource.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GraphBarrier {
    pub resource: FrameResource,
    /// The last pass that touched the resource this frame; `None` on its
    /// first use.
    pub after: Option<FramePass>,
    pub before: FramePass,
    /// Image layouts, `None` for buffers. A transient image's first use
    /// comes from `Undefined`, which discards what it held.
    pub from: Option<ImageLayout>,
    pub to: Option<ImageLayout>,
}

/// The passes a transient resource is alive in, as indices into
/// [`FrameGraph::passes`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TransientLifetime {
    pub resource: FrameResource,
    pub first: usize,
    pub last: usize,
}

impl TransientLifetime {
    /// Whether both resources are alive in one pass, so they cannot share
    /// memory.
    pub fn overlaps(&self, other: &TransientLifetime) -> bool {
        self.first <= other.last && other.first <= self.last
    }
}

/// The schedule of one frame's enabled passes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameGraph {
    pub passes: Vec<FramePass>,
    pub barriers: Vec<GraphBarrier>,
    pub transients: Vec<TransientLifetime>,
}

impl FrameGraph {
    /// Schedules the passes a frame enables, given in any order. Puts them
    /// in dependency order, places a barrier wherever a write meets another
    /// access or an image changes layout, and spans each transient
    /// resource. Persistent images arrive in their first use's layout.
    /// Reads of ambient occlusion or bloom that no pass wrote get a neutral
    /// image and no barrier. Fails when a pass is enabled twice or reads a transient resource no
    /// earlier pass wrote.
    pub fn compile(enabled: &[FramePass]) -> Result<FrameGraph, String> {
        let mut passes = enabled.to_vec();
        passes
            .sort_by_key(|pass| FramePass::ALL.iter().position(|p| p == pass));
        if let Some(pair) = passes.windows(2).find(|pair| pair[0] == pair[1]) {
            return Err(format!("{:?} is enabled twice", pair[0]));
        }
        // The last use of each resource: pass, layout and whether it wrote.
        let mut last =
            Vec::<(FrameResource, FramePass, Option<ImageLayout>, bool)>::new();
        let mut barriers = Vec::new();
        let mut transients = Vec::<TransientLifetime>::new();
        for (index, &pass) in passes.iter().enumerate() {
            let accesses = pass.accesses();
            for (i, access) in accesses.iter().enumerate() {
                let resource = access.resource;
                // A pass that reads and writes a resource uses it once.
                if accesses[..i].iter().any(|a| a.resource == resource) {
                    continue;
                }
                let write =
                    accesses.iter().any(|a| a.resource == resource && a.write);
                let layout = access.layout;
                match last.iter_mut().find(|use_| use_.0 == resource) {
                    Some(use_) => {
                        if use_.3 || write || use_.2 != layout {
                            barriers.push(GraphBarrier {
                                resource,
                                after: Some(use_.1),
                                before: pass,
                                from: use_.2,
                                to: layout,
                            });
                        }
                        *use_ = (resource, pass, layout, write);
                    }
                    None => {
                        // Frames without the pass that writes ambient
                        // occlusion or bloom bind a neutral image instead.
                        let neutral = matches!(
                            resource,
                            FrameResource::AmbientOcclusion
                                | FrameResource::Bloom
                        );
                        if !write && neutral {
                            continue;
                        }
                        if !resource.persistent() {
                            if !write {
                                return Err(format!(
                                    "{pass:?} reads {resource:?} before any pass writes it"
                                ));
                            }
                            if layout.is_some() {
                                barriers.push(GraphBarrier {
                                    resource,
                                    after: None,
                                    before: pass,
                                    from: Some(ImageLayout::Undefined),
                                    to: layout,
                                });
                            }
                        }
                        last.push((resource, pass, layout, write));
                    }
                }
                if !resource.persistent() {
                    match transients.iter_mut().find(|t| t.resource == resource)
                    {
                        Some(lifetime) => lifetime.last = index,
                        None => transients.push(TransientLifetime {
                            resource,
                            first: index,
                            last: index,
                        }),
                    }
                }
            }
        }
        Ok(FrameGraph {
            passes,
            barriers,
            transients,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_read_follows_a_write_of_the_same_resource() {
        for (index, pass) in FramePass::ALL.iter().enumerate() {
            for access in pass.accesses().iter().filter(|a| !a.write) {
                // A pass may read what it writes itself (physics state).
                let written = FramePass::ALL[..=index]
                    .iter()
                    .flat_map(|p| p.accesses())
                    .any(|a| a.write && a.resource == access.resource);
                // Indirect light reads last frame's scene color.
                let written = written
                    || (*pass == FramePass::AmbientOcclusion
                        && access.resource == FrameResource::SceneColor);
                assert!(written, "{pass:?} reads unwritten {access:?}");
            }
        }
    }

    #[test]
    fn labels_are_unique() {
        let mut labels: Vec<_> =
            FramePass::ALL.iter().map(|p| p.label()).collect();
        labels.sort();
        labels.dedup();
        assert_eq!(labels.len(), FramePass::ALL.len());
    }

    #[test]
    fn transitions_list_every_layout_change_between_passes() {
        use FrameResource::*;
        use ImageLayout::*;
        let expected = [
            (
                MaterialTextures,
                TransferDstOptimal,
                ShaderReadOnlyOptimal,
                FramePass::DepthPrepass,
            ),
            (
                SceneDepth,
                DepthStencilAttachmentOptimal,
                ShaderReadOnlyOptimal,
                FramePass::AmbientOcclusion,
            ),
            (
                ShadowMap,
                DepthStencilAttachmentOptimal,
                ShaderReadOnlyOptimal,
                FramePass::Scene,
            ),
            (
                AmbientOcclusion,
                General,
                ShaderReadOnlyOptimal,
                FramePass::Scene,
            ),
            (
                SceneDepth,
                ShaderReadOnlyOptimal,
                DepthStencilAttachmentOptimal,
                FramePass::Scene,
            ),
            (
                SceneDepth,
                DepthStencilAttachmentOptimal,
                ShaderReadOnlyOptimal,
                FramePass::DepthPyramid,
            ),
            (
                DepthPyramid,
                General,
                ShaderReadOnlyOptimal,
                FramePass::OcclusionCulling,
            ),
            (
                SceneDepth,
                ShaderReadOnlyOptimal,
                DepthStencilAttachmentOptimal,
                FramePass::LateScene,
            ),
            (
                HdrColor,
                ColorAttachmentOptimal,
                TransferSrcOptimal,
                FramePass::SceneColor,
            ),
            (
                SceneDepth,
                DepthStencilAttachmentOptimal,
                ShaderReadOnlyOptimal,
                FramePass::SceneColor,
            ),
            (
                SceneColor,
                ShaderReadOnlyOptimal,
                TransferDstOptimal,
                FramePass::SceneColor,
            ),
            (
                DepthPyramid,
                ShaderReadOnlyOptimal,
                General,
                FramePass::SceneColor,
            ),
            (
                SceneColor,
                TransferDstOptimal,
                ShaderReadOnlyOptimal,
                FramePass::Transparent,
            ),
            (
                DepthPyramid,
                General,
                ShaderReadOnlyOptimal,
                FramePass::Transparent,
            ),
            (
                HdrColor,
                TransferSrcOptimal,
                ColorAttachmentOptimal,
                FramePass::Transparent,
            ),
            (
                SceneDepth,
                ShaderReadOnlyOptimal,
                DepthStencilAttachmentOptimal,
                FramePass::Transparent,
            ),
            (
                HdrColor,
                ColorAttachmentOptimal,
                ShaderReadOnlyOptimal,
                FramePass::Bloom,
            ),
            (Bloom, General, ShaderReadOnlyOptimal, FramePass::ToneMap),
        ]
        .map(|(resource, from, to, before)| LayoutTransition {
            resource,
            from,
            to,
            before,
        });
        assert_eq!(layout_transitions(), expected);
        assert_eq!(
            FramePass::Shadow.next_layout(ShadowMap),
            Some(ShaderReadOnlyOptimal)
        );
        assert_eq!(FramePass::DebugOverlay.next_layout(Target), None);
    }

    #[test]
    fn the_graph_orders_passes_and_matches_the_declared_transitions() {
        let mut shuffled = FramePass::ALL;
        shuffled.reverse();
        shuffled.swap(3, 9);
        let graph = FrameGraph::compile(&shuffled).unwrap();
        assert_eq!(graph.passes, FramePass::ALL);
        let changes: Vec<_> = graph
            .barriers
            .iter()
            .filter_map(|b| match (b.from, b.to) {
                (Some(from), Some(to))
                    if from != to && from != ImageLayout::Undefined =>
                {
                    Some(LayoutTransition {
                        resource: b.resource,
                        from,
                        to,
                        before: b.before,
                    })
                }
                _ => None,
            })
            .collect();
        assert_eq!(changes, layout_transitions());
        // Buffers: each write orders the next use; reads after reads do not.
        let physics: Vec<_> = graph
            .barriers
            .iter()
            .filter(|b| b.resource == FrameResource::PhysicsStates)
            .map(|b| (b.after, b.before, b.from))
            .collect();
        assert_eq!(
            physics,
            [
                (Some(FramePass::Uploads), FramePass::Physics, None),
                (Some(FramePass::Physics), FramePass::Culling, None),
            ]
        );
    }

    #[test]
    fn a_frame_without_occlusion_skips_the_depth_read_transitions() {
        use FramePass::*;
        use ImageLayout::*;
        let graph =
            FrameGraph::compile(&[ToneMap, DebugOverlay, Scene, Shadow])
                .unwrap();
        assert_eq!(graph.passes, [Shadow, Scene, ToneMap, DebugOverlay]);
        let depth: Vec<_> = graph
            .barriers
            .iter()
            .filter(|b| b.resource == FrameResource::SceneDepth)
            .map(|b| (b.after, b.before, b.from, b.to))
            .collect();
        assert_eq!(
            depth,
            [
                (
                    None,
                    Scene,
                    Some(Undefined),
                    Some(DepthStencilAttachmentOptimal)
                ),
                (
                    Some(Scene),
                    DebugOverlay,
                    Some(DepthStencilAttachmentOptimal),
                    Some(DepthStencilAttachmentOptimal)
                ),
            ]
        );
        assert!(graph.barriers.iter().all(|b| !matches!(
            b.resource,
            FrameResource::AmbientOcclusion | FrameResource::Bloom
        )));
        assert_eq!(graph.transients.len(), 3, "{:?}", graph.transients);
        assert_eq!(
            FrameGraph::compile(&[ToneMap]).unwrap_err(),
            "ToneMap reads HdrColor before any pass writes it"
        );
        assert_eq!(
            FrameGraph::compile(&[Scene, Shadow, Scene]).unwrap_err(),
            "Scene is enabled twice"
        );
    }

    #[test]
    fn transients_span_their_first_to_last_use() {
        use FrameResource::*;
        let graph = FrameGraph::compile(&FramePass::ALL).unwrap();
        let spans: Vec<_> = graph
            .transients
            .iter()
            .map(|t| (t.resource, t.first, t.last))
            .collect();
        assert_eq!(
            spans,
            [
                (ShadowMap, 3, 11),
                (SceneDepth, 4, 16),
                (AmbientOcclusion, 5, 11),
                (HdrColor, 6, 14),
                (DepthPyramid, 7, 11),
                (Bloom, 12, 14),
            ]
        );
        let lifetime =
            |r| *graph.transients.iter().find(|t| t.resource == r).unwrap();
        assert!(!lifetime(Bloom).overlaps(&lifetime(ShadowMap)));
        assert!(lifetime(Bloom).overlaps(&lifetime(HdrColor)));
        assert!(lifetime(ShadowMap).overlaps(&lifetime(DepthPyramid)));
    }
}
