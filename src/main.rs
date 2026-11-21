//! English Robot Voice Assistant - native Rust version (Bevy window + 3D robot).
//!
//! SPACE        start / stop listening
//! Mouse drag   rotate the camera, mouse wheel zoom
//! Up/Down keys move the view up / down (robot moves down / up on screen)

mod answers;
mod audio;
mod tts;
mod worker;

use std::sync::{mpsc, Mutex};
use std::time::Duration;

use bevy::asset::LoadState;
use bevy::gltf::Gltf;
use bevy::input::mouse::{MouseMotion, MouseWheel};
use bevy::pbr::{NotShadowCaster, NotShadowReceiver};
use bevy::prelude::*;
use bevy::render::render_asset::RenderAssetUsages;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use bevy::window::PrimaryWindow;
use winit::window::Icon;
use bevy::winit::WinitWindows;
use rand::Rng;

use answers::Answer;
use worker::{Cmd, Ev, RobotState};

/// Tried in this order inside the `assets/` folder
const MODEL_CANDIDATES: [&str; 2] = ["robot.glb", "robot_all_animations-idle-first.glb"];
const WINDOW_ICON_FILE: &str = "icon.png";
const ROBOT_SCALE: f32 = 1.0;
const TALK_SWITCH_SECS: f32 = 4.0;

/// Starting camera distance multiplier. Bigger number = starts further away.
const START_ZOOM_OUT: f32 = 1.3;
/// Moves the robot DOWN on screen by this fraction of his height (0.1 = 10%).
/// Bigger number = robot lower on screen. Use a negative number to move him up.
const MOVE_ROBOT_DOWN: f32 = 0.1;

// ---- Logo (assets/logo.png), shown above the top-left box ----
const LOGO_FILE: &str = "logo.png";
const LOGO_TOP: f32 = 14.0; // same top margin as the right-hand panel
const LOGO_HEIGHT: f32 = 60.0; // change this to make the logo bigger or smaller
const LOGO_ASPECT: f32 = 1920.0 / 766.0; // width / height of logo.png
const LOGO_GAP: f32 = 12.0; // space between the logo and the box

// ---- Background look: near-black like the AdaTech site (#0a0907) with a soft glow ----
/// Colour at the screen edges (the website's theme colour #0a0907)
const BG_EDGE: [u8; 3] = [10, 9, 7];
/// Soft glow behind the robot
const BG_GLOW: [u8; 3] = [24, 44, 70];
/// Faint dot grid over the background
const DOT_COLOR: [u8; 3] = [150, 230, 210];
const DOT_STRENGTH: f32 = 0.10; // 0 = no dots
/// Glow on the floor under the robot's feet
const FLOOR_GLOW: [u8; 3] = [90, 220, 190];
const FLOOR_STRENGTH: f32 = 0.55; // 0 = no floor glow
/// Lowers the floor glow by this fraction of the robot's height (raise it if it floats above his feet)
const FLOOR_Y_OFFSET: f32 = 0.03;

// ------------------------------------------------------------------ resources

#[derive(Resource)]
struct WorkerTx(mpsc::Sender<Cmd>);

#[derive(Resource)]
struct WorkerRx(Mutex<mpsc::Receiver<Ev>>);

#[derive(Resource)]
struct RobotGltf(Handle<Gltf>);

#[derive(Resource)]
struct WindowIconHandle(Handle<Image>);

#[derive(Resource)]
struct Animations {
    graph: Handle<AnimationGraph>,
    idle: AnimationNodeIndex,
    talk: Vec<AnimationNodeIndex>,
}

#[derive(Resource)]
struct Talking {
    active: bool,
    last: Option<AnimationNodeIndex>,
    timer: Timer,
}

#[derive(Resource)]
struct Orbit {
    yaw: f32,
    pitch: f32,
    dist: f32,
    target: Vec3,
}

#[derive(Component, Clone, Copy, PartialEq, Eq)]
enum Slot {
    Badge,
    Status,
    Transcript,
    Reply,
    Hint,
}

/// The robot's current state as last reported by the worker (drives button visibility)
#[derive(Resource)]
struct CurrentState(RobotState);

#[derive(Component, Clone, Copy)]
enum Action {
    Mic,
    Say(&'static Answer),
}

#[derive(Component)]
struct Backdrop; // the background picture, fixed behind the robot
#[derive(Component)]
struct FloorGlow; // the glow under the robot's feet
#[derive(Component)]
struct ButtonPanel; // the info buttons on the right
#[derive(Component)]
struct MicButton;
#[derive(Component)]
struct MicLabel;

// ------------------------------------------------------------------ main

fn main() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>();
    let (ev_tx, ev_rx) = mpsc::channel::<Ev>();
    std::thread::spawn(move || worker::run_worker(cmd_rx, ev_tx));

    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "Ascend EdTech - AI Assistant".into(),
                resolution: (1100.0_f32, 720.0_f32).into(),
                ..default()
            }),
            ..default()
        }))
        .insert_resource(ClearColor(Color::srgb_u8(BG_EDGE[0], BG_EDGE[1], BG_EDGE[2])))
        .insert_resource(AmbientLight { color: Color::WHITE, brightness: 700.0, ..default() })
        .insert_resource(Orbit { yaw: 0.0, pitch: 0.15, dist: 4.0, target: Vec3::new(0.0, 1.0, 0.0) })
        .insert_resource(Talking {
            active: false,
            last: None,
            timer: Timer::from_seconds(TALK_SWITCH_SECS, TimerMode::Repeating),
        })
        .insert_resource(CurrentState(RobotState::Idle))
        .insert_resource(WorkerTx(cmd_tx))
        .insert_resource(WorkerRx(Mutex::new(ev_rx)))
        .add_systems(Startup, setup)
        .add_systems(
            Update,
            (
                keyboard,
                pump_events,
                orbit_camera,
                build_robot,
                attach_players,
                talk_switch,
                auto_frame,
                set_window_icon,
                buttons,
                update_ui_state,
                fit_backdrop,
            ),
        )
        .run();
}

// ------------------------------------------------------------------ setup

fn setup(
    mut commands: Commands,
    assets: Res<AssetServer>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
) {
    // Window icon (looked up in ./assets)
    let icon_handle: Handle<Image> = assets.load(WINDOW_ICON_FILE);
    commands.insert_resource(WindowIconHandle(icon_handle));

    // 3D model (looked up in ./assets)
    let file = MODEL_CANDIDATES
        .iter()
        .find(|n| std::path::Path::new("assets").join(n).exists())
        .copied()
        .unwrap_or("robot.glb");
    let handle: Handle<Gltf> = assets.load(file);
    commands.insert_resource(RobotGltf(handle));

    let cam = commands
        .spawn((
            Camera3d::default(),
            Transform::from_xyz(0.0, 1.3, 4.0).looking_at(Vec3::new(0.0, 1.0, 0.0), Vec3::Y),
        ))
        .id();

    // Background picture: a quad glued to the camera, far behind the robot (sized in fit_backdrop)
    let bg_mesh = meshes.add(Rectangle::new(1.0, 1.0));
    let bg_mat = materials.add(StandardMaterial {
        base_color_texture: Some(images.add(backdrop_image())),
        unlit: true,
        cull_mode: None,
        ..default()
    });
    commands.entity(cam).with_children(|c| {
        c.spawn((
            Mesh3d(bg_mesh),
            MeshMaterial3d(bg_mat),
            Transform::from_xyz(0.0, 0.0, -500.0),
            NotShadowCaster,
            NotShadowReceiver,
            Backdrop,
        ));
    });

    // Soft glow on the floor under the robot (placed in auto_frame once the robot is measured)
    let floor_mesh = meshes.add(Plane3d::default().mesh().size(1.0, 1.0));
    let floor_mat = materials.add(StandardMaterial {
        base_color_texture: Some(images.add(glow_image())),
        unlit: true,
        alpha_mode: AlphaMode::Blend,
        cull_mode: None,
        ..default()
    });
    commands.spawn((
        Mesh3d(floor_mesh),
        MeshMaterial3d(floor_mat),
        Transform::from_xyz(0.0, -100.0, 0.0),
        NotShadowCaster,
        NotShadowReceiver,
        FloorGlow,
    ));
    commands.spawn((
        DirectionalLight { illuminance: 12_000.0, shadows_enabled: true, ..default() },
        Transform::from_xyz(3.0, 5.0, 3.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));

    // Logo above the top-left box (its left edge lines up with the box)
    let logo: Handle<Image> = assets.load(LOGO_FILE);
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(LOGO_TOP),
            left: Val::Px(150.0),
            width: Val::Px(LOGO_HEIGHT * LOGO_ASPECT),
            height: Val::Px(LOGO_HEIGHT),
            ..default()
        },
        ImageNode::new(logo),
    ));

    // UI panel
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(LOGO_TOP + LOGO_HEIGHT + LOGO_GAP),
                left: Val::Px(14.0),
                width: Val::Px(430.0),
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(10.0),
                padding: UiRect::all(Val::Px(16.0)),
                ..default()
            },
            BackgroundColor(Color::srgba(0.12, 0.16, 0.23, 0.92)),
            BorderRadius::all(Val::Px(14.0)),
        ))
        .with_children(|p| {
            line(p, "Ascend EdTech Assistant", 26.0, Color::WHITE, None);
            line(p, "Click the button or press SPACE to talk.", 16.0, Color::srgb(0.58, 0.64, 0.72), None);
            button(p, "Start Mic", Action::Mic, true);
            line(p, "Idle", 16.0, Color::srgb(0.39, 0.45, 0.55), Some(Slot::Badge));
            line(p, "Status: Starting...", 18.0, Color::srgb(0.8, 0.84, 0.88), Some(Slot::Status));
            line(p, "", 14.0, Color::srgb(0.99, 0.65, 0.65), Some(Slot::Hint));
            line(p, "Your speech will appear here...", 17.0, Color::srgb(0.89, 0.91, 0.94), Some(Slot::Transcript));
            line(p, "Robot reply will appear here...", 17.0, Color::srgb(0.6, 0.85, 1.0), Some(Slot::Reply));
        });

    // Info buttons on the right (hidden while the robot is busy)
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(14.0),
                right: Val::Px(14.0),
                width: Val::Px(250.0),
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(10.0),
                padding: UiRect::all(Val::Px(14.0)),
                ..default()
            },
            BackgroundColor(Color::srgba(0.12, 0.16, 0.23, 0.92)),
            BorderRadius::all(Val::Px(14.0)),
            ButtonPanel,
        ))
        .with_children(|p| {
            line(p, "Ask me about:", 20.0, Color::WHITE, None);
for answer in answers::all() {
    button(p, answer.label, Action::Say(answer), false);
}
        });
}

fn line(p: &mut ChildBuilder, text: &str, size: f32, color: Color, slot: Option<Slot>) {
    let mut e = p.spawn((
        Text::new(text),
        TextFont { font_size: size, ..default() },
        TextColor(color),
    ));
    if let Some(s) = slot {
        e.insert(s);
    }
}

type UiQuery<'w, 's> = Query<'w, 's, (&'static mut Text, &'static mut TextColor, &'static Slot)>;

fn set_text(ui: &mut UiQuery, slot: Slot, value: &str) {
    for (mut text, _, s) in ui.iter_mut() {
        if *s == slot {
            text.0 = value.to_string();
        }
    }
}

fn set_badge(ui: &mut UiQuery, state: RobotState) {
    let (label, color) = match state {
        RobotState::Idle => ("Idle", Color::srgb(0.39, 0.45, 0.55)),
        RobotState::Listening => ("Listening", Color::srgb(0.97, 0.44, 0.44)),
        RobotState::Thinking => ("Thinking", Color::srgb(0.98, 0.75, 0.14)),
        RobotState::Talking => ("Talking", Color::srgb(0.29, 0.87, 0.5)),
    };
    for (mut text, mut c, s) in ui.iter_mut() {
        if *s == Slot::Badge {
            text.0 = format!("State: {label}");
            c.0 = color;
        }
    }
}

// ------------------------------------------------------------------ window icon

fn set_window_icon(
    images: Res<Assets<Image>>,
    winit_windows: NonSendMut<WinitWindows>,
    icon_handle: Res<WindowIconHandle>,
    mut applied: Local<bool>,
) {
    if *applied {
        return;
    }
    let Some(img) = images.get(&icon_handle.0) else {
        return;
    };
    let Ok(rgba) = img.clone().try_into_dynamic() else {
        return;
    };
    let rgba = rgba.to_rgba8();
    let (w, h) = rgba.dimensions();
    if let Ok(icon) = Icon::from_rgba(rgba.into_raw(), w, h) {
        for (_, win) in winit_windows.windows.iter() {
            win.set_window_icon(Some(icon.clone()));
        }
        *applied = true;
    }
}

// ------------------------------------------------------------------ robot model + animations

fn build_robot(
    mut commands: Commands,
    robot: Res<RobotGltf>,
    gltfs: Res<Assets<Gltf>>,
    assets: Res<AssetServer>,
    mut graphs: ResMut<Assets<AnimationGraph>>,
    mut ui: UiQuery,
    mut done: Local<bool>,
) {
    if *done {
        return;
    }
    if let LoadState::Failed(_) = assets.load_state(&robot.0) {
        *done = true;
        set_text(&mut ui, Slot::Hint, "Could not load robot.glb. Put it in the assets/ folder next to Cargo.toml.");
        return;
    }
    let Some(gltf) = gltfs.get(&robot.0) else { return };
    *done = true;

    let Some(scene) = gltf.scenes.first() else {
        set_text(&mut ui, Slot::Hint, "The .glb file has no scene.");
        return;
    };
    commands.spawn((SceneRoot(scene.clone()), Transform::from_scale(Vec3::splat(ROBOT_SCALE))));

    // Same clip logic as the web page: "idle" clip, and clips named talk / speak / speech
    let mut clips: Vec<_> = gltf.named_animations.iter().collect();
    clips.sort_by(|a, b| a.0.cmp(b.0));
    info!("Animation clips: {:?}", clips.iter().map(|c| c.0.to_string()).collect::<Vec<_>>());
    if clips.is_empty() {
        return;
    }

    let mut graph = AnimationGraph::new();
    let root = graph.root;
    let mut first = None;
    let mut idle = None;
    let mut talk = Vec::new();
    for (name, clip) in clips {
        let node = graph.add_clip(clip.clone(), 1.0, root);
        let lower = name.to_lowercase();
        first.get_or_insert(node);
        if idle.is_none() && lower.contains("idle") {
            idle = Some(node);
        } else if ["talk", "speak", "speech"].iter().any(|k| lower.contains(k)) {
            talk.push(node);
        }
    }
    if talk.is_empty() {
        warn!("No talking clips found. Adjust the names in build_robot().");
    }
    commands.insert_resource(Animations {
        graph: graphs.add(graph),
        idle: idle.or(first).unwrap(),
        talk,
    });
}

/// Measures the robot's skeleton (bone positions follow the real animated pose and scale,
/// unlike mesh bounding boxes on skinned models) and frames the camera on it once.
fn auto_frame(
    nodes: Query<&GlobalTransform, With<Name>>,
    mut orbit: ResMut<Orbit>,
    mut cam: Query<&mut Projection, With<Camera3d>>,
    mut floor: Query<&mut Transform, With<FloorGlow>>,
    mut waited: Local<u32>,
    mut done: Local<bool>,
) {
    if *done || nodes.iter().count() < 5 {
        return;
    }
    *waited += 1;
    if *waited < 30 {
        return; // let the model and its first animation pose settle
    }
    let mut min = Vec3::splat(f32::MAX);
    let mut max = Vec3::splat(f32::MIN);
    for gt in &nodes {
        let p = gt.translation();
        min = min.min(p);
        max = max.max(p);
    }
    let extent = max - min;
    info!("Robot skeleton bounds: min {min:?} max {max:?} size {extent:?}");
    if extent.max_element() < 1e-4 {
        return; // nothing useful measured yet, try again next frame
    }
    *done = true;

    // Bones stop at the neck and ankles, so add padding for the head and feet
    let size = extent.max_element() * 1.25;
    let center = (min + max) * 0.5 + Vec3::Y * extent.y * 0.05;

    // Put the floor glow under the robot's feet
    for mut t in &mut floor {
        t.translation = Vec3::new(center.x, min.y - extent.y * FLOOR_Y_OFFSET, center.z);
        t.scale = Vec3::splat(size * 0.9);
    }

    // Raising the point the camera looks at makes the robot appear lower on screen
    orbit.target = center + Vec3::Y * size * MOVE_ROBOT_DOWN;
    orbit.dist = (size * 0.5 / 0.4142 * 1.15 * START_ZOOM_OUT).max(0.1);
    if let Ok(mut proj) = cam.get_single_mut() {
        if let Projection::Perspective(p) = &mut *proj {
            p.near = (orbit.dist * 0.01).max(0.001);
            p.far = orbit.dist * 100.0;
        }
    }
}

/// Gives every animation player in the spawned model the graph and starts the idle clip.
fn attach_players(
    mut commands: Commands,
    anims: Option<Res<Animations>>,
    mut players: Query<(Entity, &mut AnimationPlayer), Without<AnimationGraphHandle>>,
) {
    let Some(a) = anims else { return };
    for (entity, mut player) in &mut players {
        let mut transitions = AnimationTransitions::new();
        transitions.play(&mut player, a.idle, Duration::ZERO).repeat();
        commands
            .entity(entity)
            .insert((AnimationGraphHandle(a.graph.clone()), transitions));
    }
}

fn play_clip(
    players: &mut Query<(&mut AnimationPlayer, &mut AnimationTransitions)>,
    node: AnimationNodeIndex,
) {
    for (mut player, mut transitions) in players.iter_mut() {
        transitions.play(&mut player, node, Duration::from_millis(300)).repeat();
    }
}

/// Random talking clip, never the same one twice in a row
fn pick_talk(a: &Animations, last: &mut Option<AnimationNodeIndex>) -> AnimationNodeIndex {
    if a.talk.is_empty() {
        return a.idle;
    }
    if a.talk.len() == 1 {
        return a.talk[0];
    }
    let options: Vec<AnimationNodeIndex> =
        a.talk.iter().copied().filter(|n| Some(*n) != *last).collect();
    let choice = options[rand::thread_rng().gen_range(0..options.len())];
    *last = Some(choice);
    choice
}

fn talk_switch(
    time: Res<Time>,
    mut talking: ResMut<Talking>,
    anims: Option<Res<Animations>>,
    mut players: Query<(&mut AnimationPlayer, &mut AnimationTransitions)>,
) {
    if !talking.active {
        return;
    }
    let Some(a) = anims else { return };
    if talking.timer.tick(time.delta()).just_finished() {
        let node = pick_talk(&a, &mut talking.last);
        play_clip(&mut players, node);
    }
}

// ------------------------------------------------------------------ input + events from the worker

fn keyboard(keys: Res<ButtonInput<KeyCode>>, tx: Res<WorkerTx>) {
    if keys.just_pressed(KeyCode::Space) {
        let _ = tx.0.send(Cmd::Toggle);
    }
}

fn pump_events(
    rx: Res<WorkerRx>,
    mut ui: UiQuery,
    anims: Option<Res<Animations>>,
    mut talking: ResMut<Talking>,
    mut cur: ResMut<CurrentState>,
    mut players: Query<(&mut AnimationPlayer, &mut AnimationTransitions)>,
) {
    let rx = rx.0.lock().unwrap();
    while let Ok(ev) = rx.try_recv() {
        match ev {
            Ev::Status(t) => set_text(&mut ui, Slot::Status, &t),
            Ev::Transcript(t) => set_text(&mut ui, Slot::Transcript, &t),
            Ev::Reply(t) => set_text(&mut ui, Slot::Reply, &t),
            Ev::Hint(t) => set_text(&mut ui, Slot::Hint, &t),
            Ev::State(state) => {
                set_badge(&mut ui, state);
                cur.0 = state;
                if let Some(a) = &anims {
                    let node = if state == RobotState::Talking {
                        talking.active = true;
                        talking.timer.reset();
                        pick_talk(a, &mut talking.last)
                    } else {
                        talking.active = false;
                        a.idle
                    };
                    play_clip(&mut players, node);
                }
            }
        }
    }
}

fn orbit_camera(
    mut motion: EventReader<MouseMotion>,
    mut wheel: EventReader<MouseWheel>,
    buttons: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    time: Res<Time>,
    mut orbit: ResMut<Orbit>,
    mut cam: Query<&mut Transform, With<Camera3d>>,
) {
    if buttons.pressed(MouseButton::Left) {
        for e in motion.read() {
            orbit.yaw -= e.delta.x * 0.005;
            orbit.pitch = (orbit.pitch + e.delta.y * 0.005).clamp(-1.2, 1.2);
        }
    } else {
        motion.clear();
    }
    for e in wheel.read() {
        orbit.dist = (orbit.dist * (1.0 - e.y * 0.1)).clamp(0.01, 100_000.0);
    }

    // Up arrow: look higher (robot moves down on screen). Down arrow: the opposite.
    let step = orbit.dist * 0.5 * time.delta_secs();
    if keys.pressed(KeyCode::ArrowUp) {
        orbit.target.y += step;
    }
    if keys.pressed(KeyCode::ArrowDown) {
        orbit.target.y -= step;
    }

    let Ok(mut t) = cam.get_single_mut() else { return };
    let rot = Quat::from_euler(EulerRot::YXZ, orbit.yaw, -orbit.pitch, 0.0);
    t.translation = orbit.target + rot * Vec3::new(0.0, 0.0, orbit.dist);
    t.look_at(orbit.target, Vec3::Y);
}
// ------------------------------------------------------------------ buttons

fn btn_normal() -> Color {
    Color::srgb(0.16, 0.34, 0.62)
}
fn btn_hover() -> Color {
    Color::srgb(0.22, 0.42, 0.74)
}
fn btn_pressed() -> Color {
    Color::srgb(0.10, 0.25, 0.50)
}
fn mic_color(state: RobotState) -> Color {
    match state {
        RobotState::Idle => btn_normal(),
        RobotState::Listening => Color::srgb(0.75, 0.20, 0.20),
        _ => Color::srgb(0.25, 0.29, 0.35),
    }
}

fn button(p: &mut ChildBuilder, label: &str, action: Action, is_mic: bool) {
    let mut e = p.spawn((
        Button,
        action,
        Node {
            padding: UiRect::axes(Val::Px(16.0), Val::Px(10.0)),
            justify_content: JustifyContent::Center,
            align_items: AlignItems::Center,
            ..default()
        },
        BackgroundColor(btn_normal()),
        BorderRadius::all(Val::Px(10.0)),
    ));
    if is_mic {
        e.insert(MicButton);
    }
    e.with_children(|b| {
        let mut t = b.spawn((
            Text::new(label),
            TextFont { font_size: 18.0, ..default() },
            TextLayout::new_with_justify(JustifyText::Center),
            TextColor(Color::WHITE),
        ));
        if is_mic {
            t.insert(MicLabel);
        }
    });
}

/// Handles clicks on the mic button and the info buttons.
fn buttons(
    mut q: Query<(&Interaction, &Action, &mut BackgroundColor), (Changed<Interaction>, With<Button>)>,
    mut cur: ResMut<CurrentState>,
    tx: Res<WorkerTx>,
) {
    for (interaction, action, mut bg) in &mut q {
        match *interaction {
            Interaction::Pressed => {
                bg.0 = btn_pressed();
                match action {
                    Action::Mic => {
                        // same as SPACE: start listening, or stop early while listening
                        if matches!(cur.0, RobotState::Idle | RobotState::Listening) {
                            let _ = tx.0.send(Cmd::Toggle);
                        }
                    }
                    Action::Say(answer) => {
                        if cur.0 == RobotState::Idle {
                            let _ = tx.0.send(Cmd::Speak {
                                title: answer.label.to_string(),
                                text: answer.text.to_string(),
                            });
                            cur.0 = RobotState::Thinking; // hide the buttons right away
                        }
                    }
                }
            }
            Interaction::Hovered => bg.0 = btn_hover(),
            Interaction::None => {
                bg.0 = match action {
                    Action::Mic => mic_color(cur.0),
                    Action::Say(_) => btn_normal(),
                }
            }
        }
    }
}

/// Hides the info buttons unless the robot is idle, and updates the mic button.
fn update_ui_state(
    cur: Res<CurrentState>,
    mut panel: Query<&mut Node, With<ButtonPanel>>,
    mut mic_bg: Query<&mut BackgroundColor, With<MicButton>>,
    mut mic_label: Query<&mut Text, With<MicLabel>>,
) {
    if !cur.is_changed() {
        return;
    }
    for mut node in &mut panel {
        node.display = if cur.0 == RobotState::Idle { Display::Flex } else { Display::None };
    }
    for mut bg in &mut mic_bg {
        bg.0 = mic_color(cur.0);
    }
    for mut text in &mut mic_label {
        text.0 = match cur.0 {
            RobotState::Idle => "Start Mic",
            RobotState::Listening => "Listening... click to stop",
            RobotState::Thinking => "Thinking...",
            RobotState::Talking => "Speaking...",
        }
        .to_string();
    }
}

// ------------------------------------------------------------------ background

/// Dark gradient with a soft glow in the middle and a faint dot grid.
fn backdrop_image() -> Image {
    const W: u32 = 1280;
    const H: u32 = 720;
    let mut data = Vec::with_capacity((W * H * 4) as usize);
    for y in 0..H {
        for x in 0..W {
            let u = x as f32 / W as f32;
            let v = y as f32 / H as f32;
            // glow centred a little above the middle, where the robot stands
            let dx = (u - 0.5) / 0.55;
            let dy = (v - 0.48) / 0.75;
            let d = (dx * dx + dy * dy).sqrt().min(1.0);
            let t = d * d * (3.0 - 2.0 * d); // smooth 0..1

            let mut c = [0.0f32; 3];
            for i in 0..3 {
                c[i] = BG_GLOW[i] as f32 * (1.0 - t) + BG_EDGE[i] as f32 * t;
            }

            // faint dot grid
            let ddx = (x % 40) as i32 - 20;
            let ddy = (y % 40) as i32 - 20;
            if ddx * ddx + ddy * ddy <= 2 {
                let a = DOT_STRENGTH * (1.0 - 0.6 * t);
                for i in 0..3 {
                    c[i] = c[i] * (1.0 - a) + DOT_COLOR[i] as f32 * a;
                }
            }
            data.extend_from_slice(&[c[0] as u8, c[1] as u8, c[2] as u8, 255]);
        }
    }
    Image::new(
        Extent3d { width: W, height: H, depth_or_array_layers: 1 },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::default(),
    )
}

/// Round glow that fades out toward the edge (used on the floor).
fn glow_image() -> Image {
    const S: u32 = 256;
    let mut data = Vec::with_capacity((S * S * 4) as usize);
    for y in 0..S {
        for x in 0..S {
            let dx = (x as f32 / S as f32 - 0.5) * 2.0;
            let dy = (y as f32 / S as f32 - 0.5) * 2.0;
            let d = (dx * dx + dy * dy).sqrt().min(1.0);
            let a = (1.0 - d) * (1.0 - d) * FLOOR_STRENGTH;
            data.extend_from_slice(&[FLOOR_GLOW[0], FLOOR_GLOW[1], FLOOR_GLOW[2], (a * 255.0) as u8]);
        }
    }
    Image::new(
        Extent3d { width: S, height: S, depth_or_array_layers: 1 },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::default(),
    )
}

/// Keeps the background quad exactly filling the window, just inside the camera's far plane.
fn fit_backdrop(
    windows: Query<&Window, With<PrimaryWindow>>,
    cam: Query<&Projection, With<Camera3d>>,
    mut bg: Query<&mut Transform, With<Backdrop>>,
) {
    let Ok(window) = windows.get_single() else { return };
    let Ok(Projection::Perspective(p)) = cam.get_single() else { return };
    let d = p.far * 0.9;
    let h = 2.0 * d * (p.fov * 0.5).tan() * 1.02;
    let aspect = window.width() / window.height().max(1.0);
    for mut t in &mut bg {
        t.translation = Vec3::new(0.0, 0.0, -d);
        t.scale = Vec3::new(h * aspect, h, 1.0);
    }
}
