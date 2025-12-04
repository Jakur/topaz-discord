use anyhow::{anyhow, Result};
use dotenv;
use lazy_static::lazy_static;
use lz_str::decompress_from_encoded_uri_component;
use once_cell::sync::{Lazy, OnceCell};
use puzzle::{Difficulty, TinueResponse};
use regex::Regex;
use std::collections::HashMap;
use std::io::Write;
use std::str::FromStr;
use std::{env, time};
use topaz_tak::board::{Board5, Board6, Board7};
use topaz_tak::proof::TinueSearch;
use topaz_tak::search::SearchInfo;
use topaz_tak::transposition_table::HashTable;
use topaz_tak::{generate_all_moves, Position, TimeBank};
use topaz_tak::{Color, GameMove, TakBoard, TakGame};

use serenity::model::channel::ReactionType;
use serenity::model::prelude::*;
use serenity::prelude::*;

use hyper_rustls::HttpsConnector;
use std::sync::{Arc, Mutex};

// mod play;
mod graph;
mod puzzle;

// use play::AsyncGameState;

lazy_static! {
    static ref HTTP_CLIENT: hyper::Client<HttpsConnector<hyper::client::HttpConnector>> = {
        let https = hyper_rustls::HttpsConnectorBuilder::new()
            .with_native_roots()
            .https_only()
            .enable_http1()
            .build();

        let client: hyper::Client<_, hyper::Body> = hyper::Client::builder().build(https);
        client
    };
    static ref ACTIVE_PUZZLES: Arc<Mutex<HashMap<UserId, puzzle::PuzzleState>>> =
        Arc::new(Mutex::new(HashMap::new()));
    static ref WHITE_RE: Regex = Regex::new(r#"Player1 "(\w|\d)+""#).unwrap();
    static ref BLACK_RE: Regex = Regex::new(r#"Player2 "(\w|\d)+""#).unwrap();
}

static TOPAZ_VERSION: OnceCell<String> = OnceCell::new();
static PUZZLE_CHANNEL: OnceCell<ChannelId> = OnceCell::new();
pub const TOPAZ_ID: UserId = UserId::new(211376698778714123);
pub const TAK_BOT_ID: UserId = UserId::new(793658103668539424);

enum GameAction {
    HandleMessage,
    RequestLink,
    None,
}

#[derive(Debug)]
struct Handler;

#[serenity::async_trait]
impl EventHandler for Handler {
    // async fn channel_create(&self, context: Context, channel: &GuildChannel) {
    //     if channel.name().contains(SHORT_NAME) {
    //         let _ = add_channel(&context, channel).await;
    //     }
    // }
    async fn message(&self, context: Context, msg: Message) {
        if msg.author.bot {
            return;
        }
        if msg.content.starts_with("!tinue") {
            tracing::debug!("Running Tinue...");
            react(&context, &msg, "👍").await;
            if let Err(e) = handle_tinue_req(&context, &msg).await {
                tracing::warn!("Error handling tinue request: {}", e);
                react(&context, &msg, "❌").await;
            }
        } else if msg.content.starts_with("!analyze_ptn")
            || msg.content.starts_with("!topaz_analyze")
        {
            tracing::debug!("Running Analysis...");
            react(&context, &msg, "👍").await;
            if let Err(e) = handle_analysis_req(&context, &msg).await {
                tracing::warn!("Error handling analysis request: {}", e);
                react(&context, &msg, "❌").await;
            }
        } else if msg.content.starts_with("!puzzle") {
            if Some(&msg.channel_id) != PUZZLE_CHANNEL.get() {
                return;
            }
            let user = msg.author.id;
            let query = msg.content.split_whitespace().nth(1).unwrap_or("medium");
            // if query.is_none() {
            //     return;
            // }
            // let query = query.unwrap();
            let puzzle_data = match query.to_lowercase().as_str() {
                "easy" => puzzle::random_puzzle(Difficulty::Easy),
                "medium" => puzzle::random_puzzle(Difficulty::Medium),
                "hard" => puzzle::random_puzzle(Difficulty::Hard),
                "insane" => puzzle::random_puzzle(Difficulty::Insane),
                _ => {
                    let id: usize = query.parse().ok().unwrap_or(0);
                    let max_puzzle = puzzle::puzzle_length();
                    if id >= max_puzzle {
                        let _ = msg
                            .reply(
                                &context,
                                format!("Please choose a puzzle between 0 and {}", max_puzzle - 1),
                            )
                            .await;
                        return;
                    }
                    let puzzle_data = puzzle::new_puzzle(id);
                    if puzzle_data.is_none() {
                        return;
                    }
                    puzzle_data.unwrap()
                }
            };
            let difficulty = puzzle_data.human_difficulty();
            let id = puzzle_data.id();
            let link = build_ninja_link(puzzle_data.build_board(), format!("Puzzle {}", id));
            {
                let mut locked = ACTIVE_PUZZLES.lock().expect("Lock is not poisoned");
                locked.insert(user, puzzle_data);
            }
            let out_message = format!("Puzzle {}\nDifficulty {}\n{}", id, difficulty, link);
            let _ = msg.reply(&context, out_message).await;
        } else if msg.content.starts_with("!solve") {
            if Some(&msg.channel_id) != PUZZLE_CHANNEL.get() {
                return;
            }
            if let Some(ptn_str) = msg.content.split_whitespace().nth(1) {
                let ptn_str = clean_ptn_move(ptn_str);
                let user = msg.author.id;
                let reply;
                if let Some(puzzle) = ACTIVE_PUZZLES.lock().unwrap().get_mut(&user) {
                    let command = &ptn_str.to_ascii_lowercase();
                    if command == "legal" {
                        let legal_moves = puzzle.legal_moves();
                        if legal_moves.is_empty() {
                            reply = String::from("None");
                        } else {
                            reply = legal_moves.join(", ");
                        }
                    } else if command == "pv" {
                        let moves = puzzle.initial_pv();
                        reply = moves.join(" ");
                    } else if command == "undo" {
                        puzzle.undo_player_move();
                        reply = "Undo completed. Note exact / valid distinction may be lost."
                            .to_string()
                    } else if command == "tps" {
                        let board = puzzle.build_board();
                        reply = format!("{:?}", board);
                    } else if command == "link" {
                        let board = puzzle.build_board();
                        reply = build_ninja_link(board, String::from("Working Solution"));
                    } else {
                        reply = String::from(
                            "Could not interpret command. To give a solution use bare ptn.",
                        )
                    }
                } else {
                    reply = format!("You have no active puzzles. Create one with !puzzle command");
                }
                let _ = msg.reply(&context, reply).await;
            }
        } else if Some(&msg.channel_id) == PUZZLE_CHANNEL.get() {
            let ptn_str = msg.content.split_whitespace().nth(0).unwrap_or("");
            let ptn_str = clean_ptn_move(ptn_str);
            let user = msg.author.id;
            let reply;
            let mut terminal = false;
            if let Some(puzzle) = ACTIVE_PUZZLES.lock().unwrap().get_mut(&user) {
                if !PTN_MOVE.is_match(&ptn_str) {
                    return;
                }
                if let Some(resp) = puzzle.user_play_move(&ptn_str) {
                    puzzle.apply_move(&ptn_str);
                    let mv = resp.inner();
                    if let Some(mv) = mv {
                        puzzle.apply_move(&mv.to_ptn::<Board6>());
                    }
                    if resp.is_terminal() {
                        terminal = true;
                    }
                    reply = tinue_move_reply(resp);
                } else {
                    reply = format!("Could not interpret {} as a legal ptn move", ptn_str);
                }
            } else if PTN_MOVE.is_match(&ptn_str) {
                reply = format!("You have no active puzzles. Create one with !puzzle command");
            } else {
                return;
            }
            let _ = msg.reply(&context, reply).await;
            if terminal {
                ACTIVE_PUZZLES.lock().unwrap().remove(&user);
            }
        } else if msg.content == "!ping" {
            tracing::debug!("Should send pong...");
            if let Err(e) = msg.channel_id.say(&context, "Pong!").await {
                tracing::warn!("Failed to send message: {:?}", e);
            }
        } else if msg.content == "!topaz version" {
            if let Some(version) = TOPAZ_VERSION.get() {
                let _ = msg.reply(&context, version).await;
            } else {
                let _ = msg.reply(&context, "Unk").await;
                tracing::warn!("Unable to find topaz version, maybe Cargo location not supplied?");
            }
        }
    }
}

fn clean_ptn_move(s: &str) -> String {
    let needs_upper = s
        .chars()
        .take(2)
        .filter(|&x| x.is_ascii_alphabetic())
        .count()
        == 2;
    if needs_upper {
        format!(
            "{}{}",
            s.chars().nth(0).unwrap().to_ascii_uppercase(),
            s.chars()
                .skip(1)
                .map(|x| x.to_ascii_lowercase())
                .collect::<String>()
        )
    } else {
        s.to_ascii_lowercase()
    }
}

fn tinue_move_reply(resp: TinueResponse) -> String {
    match resp {
        TinueResponse::ExactResponse(mv) => {
            let mv = format_move(mv);
            format!("Exact Response: {}", mv)
        }
        TinueResponse::ValidResponse(mv) => {
            let mv = format_move(mv);
            format!("Valid Response: {}", mv)
        }
        TinueResponse::UnclearResponse(mv) => {
            let mv = format_move(mv);
            format!("Unclear Response: {}", mv)
        }
        TinueResponse::PoorResponse(mv) => {
            let mv = format_move(mv);
            format!("Poor Response: {}", mv)
        }
        TinueResponse::Road => String::from("Road completed!"),
        TinueResponse::NoThreats(mv) => {
            let mv = format_move(mv);
            format!("After {} no tak threats left. Puzzle failed.", mv)
        }
    }
}

fn format_move(mv: Option<GameMove>) -> String {
    if let Some(mv) = mv {
        mv.to_ptn::<Board6>()
    } else {
        String::from("_")
    }
}

fn decompress_uri(s: &str) -> Option<String> {
    decompress_from_encoded_uri_component(s).and_then(|x| String::from_utf16(&x).ok())
}

async fn react(context: &Context, msg: &Message, unicode: &str) {
    if let Err(_) = msg
        .react(&context, ReactionType::Unicode(unicode.to_string()))
        .await
    {
        tracing::warn!("Failed to send reaction: {}", unicode);
    }
}

fn main() {
    // use std::fmt::Write;
    dotenv::dotenv().expect("Failed to load .env file");

    // puzzle::list_difficulties();
    // return;
    if let Ok(f) = env::var("CARGO_FILE") {
        if let Some(version) = read_cargo_toml(&f) {
            TOPAZ_VERSION.set(version).unwrap();
        }
    }
    if let Ok(f) = env::var("PUZZLE_CHANNEL") {
        if let Ok(chan) = ChannelId::from_str(&f) {
            PUZZLE_CHANNEL.set(chan).unwrap();
        }
    }
    tokio::runtime::Builder::new_current_thread()
        .max_blocking_threads(1)
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let subscriber = tracing_subscriber::FmtSubscriber::builder()
                // all spans/events with a level higher than TRACE (e.g, debug, info, warn, etc.)
                // will be written to stdout.
                .with_max_level(tracing::Level::WARN)
                .finish();
            tracing::subscriber::set_global_default(subscriber)
                .expect("setting default subscriber failed");
            let token = env::var("DISCORD_TOKEN").expect("Expected a token in the environment");

            let mut client = Client::builder(
                &token,
                GatewayIntents::from_bits(1 << 15 | 101376).unwrap()
                    | GatewayIntents::non_privileged(),
            )
            .event_handler(Handler)
            .await
            .unwrap();

            client.start().await.unwrap();
        });
}

lazy_static! {
    static ref PTN_META: Regex = Regex::new(r#"\[(?P<Key>.*?) "(?P<Value>.*?)"\]"#).unwrap();
    static ref PTN_MOVE: Regex = Regex::new(r#"([SCsc1-8]?[A-Ha-h]\d[+-<>]?\d*['"]*)"#).unwrap();
    static ref VER_RE: Regex = Regex::new(r#"rev = "\S+""#).unwrap();
}

// mod play;

// const TAK_TALK: ServerId = ServerId(176389490762448897);
const NODE_LIMIT: usize = 200_000;

fn read_cargo_toml(s: &str) -> Option<String> {
    let path = std::path::Path::new(s);
    let data = std::fs::read_to_string(path).ok()?;
    for line in data.lines() {
        if line.starts_with("topaz-tak") {
            if let Some(ver) = VER_RE.find(line) {
                return Some(ver.as_str().to_string());
            }
        }
    }
    None
}

struct TinueRequest<'a> {
    sender: &'a str,
    content: &'a str,
}

impl<'a> TinueRequest<'a> {
    fn new(sender: &'a str, content: &'a str) -> Self {
        Self { sender, content }
    }

    async fn get_ptn_string(&self) -> Result<String> {
        let details = self
            .content
            .split_once(" ")
            .ok_or_else(|| anyhow!("Bad length"))?
            .1;
        get_ptn_string(details).await
    }
}

async fn get_ptn_string(details: &str) -> Result<String> {
    if let Ok(game_id) = details.parse::<u32>() {
        // Assume it is a playtak id
        let mut buffer = Vec::new();
        let url = format!("https://api.playtak.com/v1/games-history/ptn/{}", game_id).parse()?;
        let mut res = HTTP_CLIENT.get(url).await?;
        while let Some(chunk) = hyper::body::HttpBody::data(&mut res.body_mut()).await {
            buffer.write_all(&chunk?)?;
        }
        // res.read_to_string(&mut buffer)?;
        Ok(String::from_utf8(buffer)?)
    } else {
        // See if it is a ptn.ninja link
        if let Ok(ninja_ptn) = parse_ninja_link(details) {
            Ok(ninja_ptn)
        } else {
            // Assume raw ptn
            Ok(details.to_string())
        }
    }
}

fn parse_ninja_link(details: &str) -> Result<String> {
    if let Some(substr) = details.split("ptn.ninja/").nth(1) {
        let part = substr
            .split("&name")
            .next()
            .ok_or_else(|| anyhow!("Bad ptn ninja link!"))?;
        // println!("{}", part);
        let decompressed =
            decompress_uri(part).ok_or_else(|| anyhow!("Bad ptn ninja game string"))?;
        dbg!(&decompressed);
        Ok(decompressed)
    } else {
        Err(anyhow::anyhow!("Bad ninja link"))
    }
}

fn sigmoid(x: f32) -> f32 {
    let ex = std::f32::consts::E.powf(x);
    ex / (1.0 + ex)
}

async fn handle_analysis_req(context: &serenity::client::Context, message: &Message) -> Result<()> {
    use std::fmt::Write;
    let req = TinueRequest::new(&message.author.name, &message.content);
    let start_time = time::Instant::now();
    let ptn = req.get_ptn_string().await?;
    let (game, moves) = parse_game(&ptn).ok_or_else(|| anyhow!("Unable to parse game"))?;
    let game = if let TakGame::Standard6(game) = game {
        game
    } else {
        anyhow::bail!("Sizes other than 6 not supported.")
    };
    anyhow::ensure!(game.komi() == 4, "Komi != 2");
    let table = HashTable::new(2 << 20);
    const MAX_MILLIS: u64 = 8_000;
    let mut out = String::new();
    // let mut out = std::fs::OpenOptions::new()
    //     .create(true)
    //     .write(true)
    //     .truncate(true)
    //     .open("analysis.ptn")
    //     .unwrap();
    let mut board = Board6::new().with_komi(4);
    if game != board {
        anyhow::bail!("Tried to start with initial tps (?), unsupported for now");
    }
    let mut info = SearchInfo::new(22, &table)
        .time_bank(TimeBank::flat(MAX_MILLIS))
        .set_max_nodes(500_000, 750_000)
        .quiet(true);
    // let mut eval = eval::Weights6::default();
    let ptn_header: Vec<&str> = ptn
        .lines()
        .take_while(|line| line.starts_with("["))
        .collect();
    for line in ptn_header {
        writeln!(&mut out, "{}", line)?;
    }
    let mut eval = topaz_tak::eval::NNUE6::default();
    let mut outcomes = Vec::new();
    for mv in moves.iter().copied() {
        info.start_search(board.ply());
        let outcome = topaz_tak::search::search(&mut board, &mut eval, &mut info).unwrap();
        outcomes.push(outcome);
        board.do_move(mv);
    }
    let mut board = Board6::new().with_komi(4);
    for (idx, (outcome, mv)) in outcomes.iter().zip(moves.iter().copied()).enumerate() {
        if idx % 2 == 0 {
            // Need number
            write!(&mut out, "{}. ", idx / 2 + 1)?;
        }
        let mut mark = "";
        let next_outcome = outcomes.get(idx + 1);

        let pv: Vec<_> = outcome
            .pv
            .clone()
            .into_iter()
            .map(|x| x.to_ptn::<Board6>())
            .take(6)
            .collect();

        if let Some(next) = next_outcome {
            let mut win_pct = sigmoid(next.score() as f32 / 400.0);
            // let mut win_pct = (next.score() as f32 / 500.0).clamp(-1.0, 1.0);
            // Backwards because we are looking at the *next* eval
            if board.side_to_move() == Color::White {
                win_pct = 1.0 - win_pct;
            }
            // win_pct = (win_pct + 1.0) / 2.0;
            win_pct *= 100.0;
            if outcome
                .best_move()
                .map(|x| x.trim_end_matches(r"*").to_string())
                != Some(mv.to_ptn::<Board6>().trim_end_matches(r"*").to_string())
            {
                // Did not play the engine move
                let next_score = -1 * next.score();
                let score = outcome.score();
                let diff = (next_score - score).abs();
                let mult = if score.abs() < 500 {
                    1
                } else if score.abs() < 1200 {
                    2
                } else {
                    3
                };
                // dbg!(mv.to_ptn::<Board6>());
                // dbg!(outcome.score());
                // dbg!(next_score);
                if false {
                } else if next_score > score && diff > mult * 100 {
                    mark = "!!";
                } else if next_score > score && diff > mult * 35 {
                    mark = "!";
                } else if next_score < score && diff > mult * 250 {
                    mark = "??";
                } else if next_score < score && diff > mult * 90 {
                    mark = "?";
                }
            }
            write!(
                &mut out,
                "{}{} {{{:.1}%, pv {}}} ",
                mv.to_ptn::<Board6>(),
                mark,
                win_pct,
                pv.join(" ")
            )?;
        } else {
            write!(
                &mut out,
                "{} {{pv {}}}",
                mv.to_ptn::<Board6>(),
                pv.join(" ")
            )?;
        }

        if idx % 2 == 1 {
            writeln!(&mut out, "")?;
        }

        board.do_move(mv);

        if board.game_result().is_some() {
            break;
        }
    }
    let plot = graph::generate_graph(out.as_bytes());
    let white_name = WHITE_RE
        .find(&ptn)
        .map(|mat| mat.as_str())
        .unwrap_or("WHITE");
    let black_name = BLACK_RE
        .find(&ptn)
        .map(|mat| mat.as_str())
        .unwrap_or("BLACK");
    let ptn_ninja_message = send_ninja_short_link(&out).await?;
    if let Ok(plot) = plot {
        let create_attach = serenity::builder::CreateAttachment::bytes(plot, "plot.png");
        let create_message = serenity::builder::CreateMessage::new().content(format!(
            "Finished analyzing {} vs {} in {:.1}s. {}",
            white_name,
            black_name,
            start_time.elapsed().as_secs_f32(),
            ptn_ninja_message,
        ));
        message
            .channel_id
            .send_files(context, [create_attach], create_message)
            .await?;
    } else {
        let create_message = serenity::builder::CreateMessage::new().content(format!(
            "Failed generating plot. Finished analyzing {} vs {} in {:.1}s. {}",
            white_name,
            black_name,
            start_time.elapsed().as_secs_f32(),
            ptn_ninja_message,
        ));
        message
            .channel_id
            .send_message(context, create_message)
            .await?;
    }
    Ok(())
}

async fn send_ninja_short_link(analysis: &str) -> Result<String> {
    use hyper::body::HttpBody;
    let json = serenity::json::json!({
        "ptn": analysis
    });
    let req = HTTP_CLIENT
        .request(
            hyper::Request::builder()
                .method(hyper::Method::POST)
                .uri("https://url.ptn.ninja/short")
                .header("content-type", "application/json")
                .body(hyper::Body::from(json.to_string()))?,
        )
        .await?;
    let mut body = req.into_body();
    let data = body.data().await;
    if let Some(data) = data {
        dbg!(&data);
        let data = data?;
        let data = str::from_utf8(&data)?;
        Ok(data.to_string())
    } else {
        anyhow::bail!("No response?");
    }
}

async fn handle_tinue_req(context: &serenity::client::Context, message: &Message) -> Result<()> {
    let req = TinueRequest::new(&message.author.name, &message.content);
    let start_time = time::Instant::now();
    let from_tps = TakGame::try_from_tps(
        message
            .content
            .split_once(" ")
            .ok_or_else(|| anyhow!("Missing space in tinue request"))?
            .1,
    );
    let (game, moves) = if let Ok(board) = from_tps {
        (board, Vec::new())
    } else {
        let ptn = req.get_ptn_string().await?;
        parse_game(&ptn).ok_or_else(|| anyhow!("Unable to parse game"))?
    };
    if moves.len() <= 5 {
        // Interpret as a single position
        match game {
            TakGame::Standard5(board) => find_one_tinue(board, context, message).await?,
            TakGame::Standard6(board) => find_one_tinue(board, context, message).await?,
            TakGame::Standard7(board) => find_one_tinue(board, context, message).await?,
            _ => anyhow::bail!("Unsupported board size"),
        }
        return Ok(());
    }
    let tinue_plies = match game {
        TakGame::Standard5(board) => find_all_tinue(board, &moves),
        TakGame::Standard6(board) => find_all_tinue(board, &moves),
        TakGame::Standard7(board) => find_all_tinue(board, &moves),
        _ => anyhow::bail!("Unsupported board size"),
    };

    let mut tinue = Vec::new();
    let mut road = Vec::new();
    let mut timeout = Vec::new();
    for ply in tinue_plies.into_iter() {
        match ply {
            TinueStatus::Tinue(_) => tinue.push(ply.to_string()),
            TinueStatus::Road(_) => road.push(ply.to_string()),
            TinueStatus::Timeout(_) => timeout.push(ply.to_string()),
        }
    }
    let printable = |vec: Vec<String>| {
        if vec.len() == 0 {
            "None".to_string()
        } else {
            vec.join(", ")
        }
    };
    let duration = time::Instant::now().duration_since(start_time);
    let message_string = format!(
        "Sure thing, {}! Completed in {} ms.\nTinue: {}\nRoad: {}\nTimeout: {}",
        message.author.name,
        duration.as_millis(),
        printable(tinue),
        printable(road),
        printable(timeout),
    );
    message
        .channel_id
        .say(context.http.clone(), message_string)
        .await?;
    Ok(())
}

#[derive(Debug, Clone, Copy)]
enum TinueStatus {
    Tinue(usize),
    Road(usize),
    Timeout(usize),
}

impl std::fmt::Display for TinueStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
        let ply = match self {
            TinueStatus::Tinue(x) => x,
            TinueStatus::Road(x) => x,
            TinueStatus::Timeout(x) => x,
        };
        let color = if ply % 2 == 0 { "W" } else { "B" };
        let move_num = ply / 2;
        write!(f, "{}{}", move_num, color)
    }
}

fn thread_search<T: TakBoard + std::fmt::Debug>(board: T) -> Result<Option<bool>> {
    let mut search = TinueSearch::new(board).limit(NODE_LIMIT).quiet();
    let tinue = search.is_tinue();
    if search.aborted() {
        tracing::debug!("Aborting search on: {:?}", search.board);
        // Todo
        return Ok(None);
    }
    let tinue = tinue.ok_or_else(|| anyhow!("Failed to execute Tinue check"))?;
    tracing::debug!("Valid Tinue Result Received: {:?}", search.board);
    // Build temp file

    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open("proof-data.txt")?;
    let mut file = std::io::BufWriter::new(file);
    let mut hist = Vec::new();
    let mut zobrist_hist = std::collections::HashSet::new();
    search.rebuild(
        &mut file,
        &mut hist,
        &mut zobrist_hist,
        search.is_attacker(),
    )?;
    file.flush()?;
    // Build svg
    let reader = std::io::BufReader::new(std::fs::File::open("proof-data.txt").unwrap());
    let writer = std::io::BufWriter::new(
        std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open("tinue.svg")?,
    );
    inferno::flamegraph::handle_file(reader, writer)?;
    tracing::debug!("Handled file!");
    Ok(Some(tinue))
}

async fn find_one_tinue<T: TakBoard + std::fmt::Debug + Send + 'static>(
    board: T,
    context: &Context,
    message: &Message,
) -> Result<()> {
    // let tinue = thread_search(board.clone());
    let tinue = tokio::task::spawn_blocking(move || thread_search(board)).await??;
    if let Some(tinue) = tinue {
        let f1 = tokio::fs::OpenOptions::new()
            .read(true)
            .open("tinue.svg")
            .await?;
        let st = if tinue {
            "Tinue Found!"
        } else {
            "No Tinue Found."
        };
        let create_attach = serenity::builder::CreateAttachment::file(&f1, "tinue.svg").await?;
        let create_message = serenity::builder::CreateMessage::new().content(format!(
            "{}\n{}",
            st, "Open this file in a web browser for best results."
        ));
        message
            .channel_id
            .send_files(context, [create_attach], create_message)
            .await?;
    } else {
        message.channel_id.say(context, "Timed out. Sorry.").await?;
    }
    // messag
    // if let Some(is_tinue) = search.is_tinue() {
    //     if is_tinue {
    //         let road_move = find_road_move(&mut search.board);
    //         if road_move.is_some() {
    //             println!("{}: Road", s);
    //             return Some(TinueStatus::Road(ply_index));
    //         } else {
    //             println!("{}: Tinue", s);
    //             return Some(TinueStatus::Tinue(ply_index));
    //         }
    //     } else {
    //         println!("{}: Not Tinue", s);
    //         return None;
    //     }
    // } else {
    //     println!("{}: Timeout", s);
    //     println!("Timeout TPS: {:?}", search.board);
    //     return Some(TinueStatus::Timeout(ply_index));
    // }
    Ok(())
}

fn find_all_tinue<T: TakBoard + std::fmt::Debug>(
    mut board: T,
    moves: &[GameMove],
) -> Vec<TinueStatus> {
    let mut vec = Vec::new();
    for (idx, mv) in moves.iter().enumerate() {
        if idx < 6 {
            board.do_move(*mv);
            continue;
        }
        let s = move_s(idx);
        let mut search = TinueSearch::new(board).limit(NODE_LIMIT).quiet();
        if let Some(is_tinue) = search.is_tinue() {
            if is_tinue {
                let road_move = find_road_move(&mut search.board);
                if road_move.is_some() {
                    vec.push(TinueStatus::Road(idx + 2));
                    tracing::debug!("{}: Road", s);
                } else {
                    vec.push(TinueStatus::Tinue(idx + 2));
                    tracing::debug!("{}: Tinue", s);
                }
            } else {
                tracing::debug!("{}: Not Tinue", s);
            }
        } else {
            vec.push(TinueStatus::Timeout(idx + 2));
            tracing::debug!("{}: Timeout\nTimeout TPS {:?}", s, search.board);
        }
        board = search.board;
        board.do_move(*mv);
    }
    vec
}

fn find_road_move<B: TakBoard>(board: &mut B) -> Option<GameMove> {
    let mut moves = Vec::new();
    let side = board.side_to_move();
    generate_all_moves(board, &mut moves);
    let road = moves.into_iter().find(|mv| {
        let rev = board.do_move(*mv);
        let road = board.road(side);
        board.reverse_move(rev);
        road
    });
    road
}

fn move_s(idx: usize) -> String {
    let color = if idx == 0 {
        "B"
    } else if idx == 1 {
        "W"
    } else if idx % 2 == 0 {
        "W"
    } else {
        "B"
    };
    format!("{}. {}", idx / 2, color)
}

fn parse_move(mv_str: &str, size: usize, color: Color) -> Option<GameMove> {
    let mut iter = mv_str.chars().take_while(|&c| c != '\'' && c != '"');
    let first = iter.next()?;
    let mv = if first == 'S' || first == 's' {
        let mut s = String::new();
        // Lowercase everything besides this first S
        s.push(first.to_ascii_uppercase());
        while let Some(c) = iter.next() {
            s.push(c.to_ascii_lowercase());
        }
        s
    } else if first == 'C' || first == 'c' {
        let mut s = String::new();
        // We need to find out if this C is a capstone or a bad square indicator
        let second = iter.next()?;
        if second.is_ascii_alphabetic() {
            // The previous must have been a capstone, make it capitalized
            s.push(first.to_ascii_uppercase());
            s.push(second.to_ascii_lowercase());
        } else {
            // The second must be a number, meaning this should be lowercase c
            s.push(first.to_ascii_lowercase());
            s.push(second);
        }
        while let Some(c) = iter.next() {
            s.push(c.to_ascii_lowercase());
        }
        s
    } else {
        let mut string = String::new();
        string.push(first.to_ascii_lowercase());
        for c in iter.map(|c| c.to_ascii_lowercase()) {
            string.push(c);
        }
        string
    };
    let mv = GameMove::try_from_ptn_m(&mv, size, color)?;
    Some(mv)
}

fn parse_game(full_ptn: &str) -> Option<(TakGame, Vec<GameMove>)> {
    let mut meta = HashMap::new();
    let mut moves = Vec::new();
    let mut color = Color::White;
    for m in PTN_META.captures_iter(&full_ptn) {
        meta.insert(m["Key"].to_string(), m["Value"].to_string());
    }
    let size = meta.get("Size")?.parse().ok()?;
    let moves_text = full_ptn.split("]").last()?;
    for m in PTN_MOVE.captures_iter(&moves_text) {
        let mv = parse_move(&m[0], size, color)?;
        moves.push(mv);
        color = !color;
    }
    let mut komi = 0;
    if let Some(k) = meta.get("Komi") {
        komi = match k.as_str() {
            "0" => 0,
            "0.5" => 1,
            "1" => 2,
            "1.5" => 3,
            "2" => 4,
            "2.5" => 5,
            "3" => 6,
            _ => 0,
        };
    }
    if let Some(tps) = meta.get("TPS") {
        tracing::debug!("TPS: {}", tps);
        let game = TakGame::try_from_tps(tps).ok()?;
        let game = match game {
            TakGame::Standard5(b) => TakGame::Standard5(b.with_komi(komi)),
            TakGame::Standard6(b) => TakGame::Standard6(b.with_komi(komi)),
            TakGame::Standard7(b) => TakGame::Standard7(b.with_komi(komi)),
            _ => return None,
        };
        if let Color::Black = game.side_to_move() {
            for i in 0..moves.len() {
                let mv = moves[i];
                if mv.is_place_move() {
                    moves[i] =
                        GameMove::from_placement(mv.place_piece().swap_color(), mv.src_index());
                }
            }
        }
        return Some((game, moves));
    }
    match size {
        5 => Some((TakGame::Standard5(Board5::new().with_komi(komi)), moves)),
        6 => Some((TakGame::Standard6(Board6::new().with_komi(komi)), moves)),
        7 => Some((TakGame::Standard7(Board7::new().with_komi(komi)), moves)),
        _ => None,
    }
}

fn build_ninja_link(board: Board6, name: String) -> String {
    let move_num = board.move_num();
    let s = format!(
        "[Player1 \"White\"]\n[Player2 \"Black\"]\n[Site \"ptn.ninja\"]\n[TPS \"{:?}\"]\n[Opening \"swap\"]\n\n{}.\n", board, move_num
    );
    let link = format!(
        "https://ptn.ninja/{}&name={}",
        lz_str::compress_to_encoded_uri_component(&s),
        lz_str::compress_to_encoded_uri_component(&name)
    );
    link
}

#[cfg(test)]
mod test {
    use super::*;
    #[test]
    fn uri() {
        let data = "NoBQNghgngpgTgRgAQCIDqALAlgFxigXQFgAoUSWOAJlQCFIBjAa0NOAGVcZUAHHAOwB0-LPwBWEVmU4AvbigBsU4ABUQ7VAA8ANAgRVtmgCwB6HfqpU9l7QZtWAwrZMH9twwGYzu9wh9V2EzcEdkMfP01vfSdNV10kGioATmUAeR4YEX4Ac1QAZwB3CB4pUmTBIA&name=OoCwlgLgpgBAbgZxgIQDYEMDGBrGA2ADzyA";
        // let data = "NoBQNghgngpgTgRgAQCIDqALAlgFxigXQFgAoUSWOAJlQCFIBjAa0NOAGVcZUAHHAOwB0-LPwBWEVmU4AvbigBsU4ABUQ7VAA8ANAgRVtmgCwB6HfqpU9l7QZtWAwrZMH9twwGYzu9wh9V2EzcEdkMfP01vfSdNV10kGioATmUAeR4YEX4Ac1QAZwB3CB4pUmTBIA&name=AoVwXmA2CmAECMQ";
        // let data = "NoBQNghgngpgTgRgAQCIDqALAlgFxigXQFgAoUSWOAJlQCFIBjAa0NOAGVcZUAHHAOwB0-LPwBWEVmU4AvbigBsU4ABUQ7VAA8ANAgRVtmgCwB6HfqpU9l7QZtWAwrZMH9twwGYzu9wh9V2EzcEdkMfP01vfSdNV10kGioATmUAeR4YEX4Ac1QAZwB3CB4pUmTBIA&name=OoCwlgLgpgBAbgZxgIQDYEMDGBrGA2ADzxgAoBmAJgEog";
        let text = decompress_uri(data).unwrap();
        println!("{}", text);
        let compressed = lz_str::compress_to_encoded_uri_component(&text);
        println!("{}", compressed);
        println!("{}", decompress_uri(&compressed).unwrap());
        println!("{}", decompress_uri("AoVwXmA2CmAECMQ").unwrap());
    }
    // #[test]
    // fn tak_tinue_marks() {
    //     let s1 = concat!(
    //         "!tinue https://ptn.ninja/NoEQhgLgpgBARAJgAwIQOhWgjAdjgXQFgAoYAZQEtp4ALCCABwGcAuAejYHMqaBXAIzQBjAPYBbN",
    //         "nzFgAdpDYQwAawBCIiARLAACgBswATygAnLPAAqIhmABeajUVJ7DJhPACC-A0yabSlG1g4ADY-YAB5BigZChlOeCYAdzAGP",
    //         "xIsNBgAM2CYMCwSdBgAEwAWGCgAVhIAZkyocuLq4lLMoXKAYWKakkq2yph+ZuDM-nKoHuIcNpqSmoBqEgAOTO6K4JIATlHZq",
    //         "AR0pEyOrN2agB50jJK3YqwAciA&name=CoewDghgXgQiAuACAbgZ0QQQEYE9XoDYAPAxAJgAYyyA6KmgRgHYg"
    //     );
    //     let t = TinueRequest::new("", &s1);
    //     let ptn = t.get_ptn_string().unwrap();
    //     let parsed = parse_game(&ptn);
    //     assert!(parsed.is_some());
    // }
    // #[test]
    // fn start_from_tps_black() {
    //     let s1 = concat!(
    //         "https://ptn.ninja/NoFQCgygBARATAGgIwMUpa5IggHgehRThKwGFlUc581UTsrkNGC7F3DVn04L1KSWtw4YcRKHCgAWOD",
    //         "AC6wACIBDAC4BTWHAAMJAHQ7p+kgtABLALZaYOpAC4AzADZ7SHWYjnNsABZq1AAcAZ3t8fABzb18AVwAjfQBjAHtLfFjLFQA7",
    //         "dXw1FQBrACFktTMwABsVAE8NACckWBBkwJUALxKyxUqa+qkYFQrzZKy9T3M2m2czAGlU820zAHlAjSzzLIjYYIB3FUCFKEcAE2",
    //         "cAWhlE6QuAM2cjjWkAPigro5vpAGoMV+cAHkkGn+AHIjsckE8fsc4J9QcdrlB4aC4MdHJ8gA"
    //     );
    //     let tps = "2,12,x,21S,1,221S/1,1,22221C,1112,2S,21/2,1,2,112S,1,x/2,1,22221S,2,1,2/1,2,11112C,1,1,1/2,2,2,x,12,112S 1 49";
    //     let ptn = get_ptn_string(s1).unwrap();
    //     let (mut game, moves) = parse_game(&ptn).unwrap();
    //     for mv in moves {
    //         game.do_move(mv);
    //     }
    //     match game {
    //         TakGame::Standard6(board) => {
    //             let s = format!("{:?}", board);
    //             assert_eq!(tps, s);
    //             println!("{}", s);
    //         }
    //         _ => assert!(false),
    //     }
    // }
    // #[test]
    // fn ptn_ninja() {
    //     let s1 = concat!(
    //         "!tinue https://ptn.ninja/NoEQhgLgpgBARAJgAwIQOiQRjZgHHAXWABUBLAW1jk0wC4EBmWgVmcOAGVTp4ALCCAAcAzr",
    //         "QD0YgObdeAVwBGaAMYB7cmLnkwAO0hiIYANYAhFRHYAFADZgAnlABOmeGAdLeANyjbSwldssbewcEeAAzAHcoiNJ5WPYuAC8q",
    //         "AHZ2AGk1UngEdgB5QW9SbUl4YQiwQUIYMGdjZwATABYYJQZWlob2qBaodvkW+XaAMRaAYQbmGDD2sNCxqFC+gGoYAFF2ham1hF",
    //         "WoZgBaGEkWtamwTsOYYanhgDYYBbugA"
    //     );
    //     let s2 = concat!(
    //         "!tinue https://ptn.ninja/NoZQlgLgpgBARABQDYEMCeAVFBrAdAYwHsBbOAXQFgAoYAUQDcoA7CeAeSaTCdmXXOrAAwkkL5",
    //         "s8AIwBWAFwAGGAGoAzPIE0ASlADOAVySs4mgLTrKNcAC9YcAOwbgAMVQQd8ACznBQlAAd3OAAmRzY-Zm4Ac3gdAHd-RwwEEHgADw",
    //         "AaSUzJSSD0vIL8gHoMoKF01I9irJygyXKs1JVi1Lza7NzMiqCStq6azJ7UgDZMosbhmEkYII8NalncIA&name=CIQwlgNgngBACgV",
    //         "wF5IgUxgYgIwGYBsAdLjABQBCaA5mAHa1oBOAlEA&showPTN=false&theme=MYGwhgzhCWxA"
    //     );
    //     let s3 = concat!(
    //         "!tinue https://ptn.ninja/NoEQhgLgpgBARAJgAwIQOiQRjZgzHAXWABUBLAW1jkwA4AuXXOgVgHZDgBlU6eACwgQADgGc6AenE",
    //         "BzHnwCuAIzQBjAPblx88mAB2kcRDABrAEKqIHAAoAbMAE8oAJ0zwwAKwAepK7YeOE8DoAjk6qOhzcAF5UAGwcANLqpPAIHADyQlA6pD",
    //         "pS8CIA7mBCHOC8iCjoWGgIACwcZJTweHRItQw0HABKUCJy1hDwSAC0AGKEMACCLmAxMADCzDAKS-O1y+vzuDDKASbbCgGT2-MAJtsmc",
    //         "6cB83MKLvNQAVDbAKIIADw7CADUMJy7GAAMxOvxg5y+CGuf0BIACpxcAJcTwWmAAfMD1qMllAlq8XKd1q91rjhjB9mSEMpcH8oTSYGAl",
    //         "pwQAdMLSETCXCAZsw-rhrh9MAEqbVhpgXPzahjcLsfkKYCyvspRTBmFBaoL1ucFSrrjrwWCQCr9gqApxCQqloSybiYLVzj9UDBcIc-vc",
    //         "YCJAU8vhydrgvrVlWTJqtPv9YQTcGTzdswEco3brminSy-nrJWKAkCJUmDXantLTswvsTwesYkDfpglpWYCNRkA",
    //     );
    //     let s4 = concat!(
    //         "!tinue https://ptn.ninja/NoEQhgLgpgBARAJgAwIQOiQRjQgzHAXWABUBLAW1jkwE4AuAFgDY6EHDgBlU6eACwgQADgGc6AenEBzHn",
    //         "wCuAIzQBjAPblx88mAB2kcRDABrAEKqIHAAoAbMAE8oAJ0zwAslD6OA4qWvWRVrYOjghuumDEHNwAXlRMHADS6qTwCBwA8kJQOqQ6UvAiAO",
    //         "5gQoQwAIIuCi4mocqhJrgwAMIKDC0uACahIFUMALQVoVChAKIuAGYuAGKhCAq4g+UIg1MAPC1TMJxgobMbu4PTmBudTaNNUO0g7Z0ArDCjD",
    //         "-eDnMrtnFBMLQwAfDALIYA9rNJpgJoIKCYADUAIe5XaYAeuE6DDWmFCqJgDBGa1QMGUDxAD2U3xA30+oWafxgZ1h1JguJaCH+uDAmF+GJgCG",
    //         "6G1wCgQ0IQLk4AqAA"
    //     );
    //     let s5 = concat!(
    //         "!tinue https://ptn.ninja/NoEQhgLgpgBARAJgAwIQOiQZg5uBdYAFQEsBbWOARgHYAuSgVloQf2AGVjp4ALCCAA4BnWgHpRAcy48ArgC",
    //         "M0AYwD2pUbNJgAdpFEQwAawBCyiGwAKAGzABPKACdK8ALLawhC9bv2E8c2BlLNk4ALwoANjYAaVVieAQ2AHkBKC1iLQl4IQB3MAF8GDAnADN",
    //         "wmAATBgqAFhhFKsVauSqQMqgq8vCAWhgoWoBRTBgAYXKhuVqxgGo6scKG6pnGkaGEcsWYIWKqzEqAHhhtg86YfrLi2vZ1mDkhgEFx3wAxIcaD",
    //         "4cwD9kV73x+DuRORRA3zlP5DBj1LqUSaYHpyMpgBAAPhgmFu0N8awQx18QnqFVeDAO9SAA"
    //     );
    //     let s6 = concat!(
    //         "!tinue https://ptn.ninja/NoEQhgLgpgBARAJgAwIQOiQZjQRiXAXWABUBLAW1kRwC4cB2GgVnsOAGVTp4ALCCAA4BnGgHpRAcy48ArgCM",
    //         "0AYwD25UbPJgAdpFEQwAawBCyiGwAKAGzABPKACcc8AIJybQoRet37CeMWUBMAAvEzMiTmCqADY2AGlVUnh0JjYAeQEoLVItCXghAHcwAUIgA"
    //     );
    //     let s7 = concat!(
    //         "!tinue https://ptn.ninja/NoEQhgLgpgBARAJgAwIQOiQZjQRgXAXQFgAoYAFQEsBbWOHANgC4BWFpzTQ04AZUujwAFhAgAHAM5MA9NIDmA",
    //         "oQFcARmgDGAe2rTl1MADtI0iGADWAIU0RuZAAoAbMAE8oAJxzwkADxYB2BgBOBnUQ22BHF3cEeHJNMTAALysbYjJ+RLoGcIBpbUp4fDTgAHkxK",
    //         "ANKAzl4CQB3MDFbUhw0GDAcIA&name=AwDwrA7AbAnFDGCAEA3AzkgKgewA4EMAvAIWwBckoQokAmYW2gOmAGYmBGWgWg6ibBgmrVkA"
    //     );
    //     let komis = [0, 0, 0, 0, 4, 5];
    //     for (idx, s) in [s1, s2, s3, s4, s5, s6, s7].iter().enumerate() {
    //         let t = TinueRequest::new("", &s);
    //         let ptn = t.get_ptn_string().unwrap();
    //         let parsed = parse_game(&ptn);
    //         if s == &s3 {
    //             if let Some((_, ref moves)) = parsed {
    //                 let count = moves
    //                     .iter()
    //                     .filter(|m| &m.to_ptn::<Board6>() == "Sc2")
    //                     .count();
    //                 assert_eq!(count, 3);
    //             }
    //         }
    //         if idx == 4 || idx == 5 {
    //             let game = &parsed.as_ref().unwrap().0;
    //             match game {
    //                 TakGame::Standard6(g) => {
    //                     assert_eq!(g.komi(), komis[idx]);
    //                 }
    //                 _ => assert!(false),
    //             }
    //         }
    //         assert!(parsed.is_some());
    //     }
    // }
}
