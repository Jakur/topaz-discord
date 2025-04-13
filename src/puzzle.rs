use super::Board6;
use lazy_static::lazy_static;
use rand::seq::SliceRandom;
use std::fs::read_to_string;
use topaz_tak::{generate_all_moves, proof::TinueSearch, GameMove, Position, TakBoard};

pub fn new_puzzle(id: usize) -> Option<PuzzleState> {
    // Todo make sure id is valid
    Some(PuzzleState::new(id, PUZZLES.get(id)?))
}

pub fn random_puzzle(difficulty: Difficulty) -> PuzzleState {
    DIFFICULTIES.choose(difficulty)
}

pub fn puzzle_length() -> usize {
    PUZZLES.len()
}

lazy_static! {
    static ref DIFFICULTIES: Difficulties = Difficulties::new();
    static ref PUZZLES: Vec<PuzzleData> = {
        let data = read_to_string("tinue_data.csv").unwrap();
        data.lines()
            .skip(1)
            .enumerate()
            .map(|(puzzle_id, line)| {
                let split: Vec<_> = line.split(";").collect();
                let id = split[0];
                let tps = split[1].to_string();
                let pv = split[2];
                let pv = pv.split(" ").map(|x| x.to_string()).collect::<Vec<_>>();
                let nodes = split[3].parse::<f32>().unwrap();
                // let root_nodes = split[4].parse::<f32>().unwrap();
                let difficulty = nodes.log2();
                PuzzleData {
                    puzzle_id,
                    game_id: id.parse().unwrap(),
                    tps,
                    pv,
                    difficulty,
                }
            })
            .collect()
    };
}

struct Difficulties {
    data: [Vec<u16>; 4],
}

impl Difficulties {
    fn new() -> Self {
        let mut data = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
        for (pid, p) in PUZZLES.iter().enumerate() {
            let idx = p.human_difficulty() as usize;
            data[idx].push(pid as u16);
        }
        Self { data }
    }
    fn choose(&self, diff: Difficulty) -> PuzzleState {
        let mut rng = rand::thread_rng();
        let slice = &self.data[diff as usize];
        let idx = slice.choose(&mut rng).unwrap_or(&0);
        new_puzzle(*idx as usize).unwrap()
    }
}

pub struct PuzzleState {
    puzzle_num: usize,
    original_game: usize,
    active_moves: Vec<String>,
    active_pv: Vec<String>,
    is_tinue: bool,
}

#[derive(Clone, Copy)]
pub enum TinueResponse {
    ExactResponse(Option<GameMove>),
    ValidResponse(Option<GameMove>),
    UnclearResponse(Option<GameMove>),
    PoorResponse(Option<GameMove>),
    Road,
    NoThreats(Option<GameMove>),
}

impl TinueResponse {
    pub fn inner(&self) -> Option<GameMove> {
        match self {
            TinueResponse::ExactResponse(mv)
            | TinueResponse::ValidResponse(mv)
            | TinueResponse::UnclearResponse(mv)
            | TinueResponse::PoorResponse(mv)
            | TinueResponse::NoThreats(mv) => *mv,
            TinueResponse::Road => None,
        }
    }
    pub fn is_terminal(&self) -> bool {
        match self {
            TinueResponse::Road | TinueResponse::NoThreats(_) => true,
            _ => false,
        }
    }
}

impl PuzzleState {
    fn new(puzzle_num: usize, data: &PuzzleData) -> Self {
        Self {
            puzzle_num,
            original_game: data.game_id,
            active_moves: Vec::new(),
            active_pv: data.pv.clone(),
            is_tinue: true,
        }
    }
    pub fn id(&self) -> usize {
        PUZZLES[self.puzzle_num].puzzle_id
    }
    pub fn initial_pv(&self) -> &Vec<String> {
        &PUZZLES[self.puzzle_num].pv
    }
    pub fn initial_tps(&self) -> String {
        PUZZLES[self.puzzle_num].tps.clone()
    }
    pub fn initial_side(&self) -> topaz_tak::Color {
        let board = Board6::try_from_tps(&self.initial_tps()).unwrap();
        board.side_to_move()
    }
    pub fn build_board(&self) -> Board6 {
        // Todo eventually make this work with other sizes
        let mut board = Board6::try_from_tps(&PUZZLES[self.puzzle_num].tps).unwrap();
        for mv in self.active_moves.iter() {
            let mv = GameMove::try_from_ptn(mv, &board).unwrap();
            board.do_move(mv);
        }
        board
    }
    pub fn apply_move(&mut self, game_move: &str) {
        self.active_moves.push(game_move.to_string());
    }
    pub fn undo_player_move(&mut self) {
        self.active_moves.pop();
        self.active_moves.pop();
        if self.active_moves.len() == 0 {
            self.active_pv = self.initial_pv().clone();
        } else {
            self.active_pv = Vec::new();
        }
    }
    pub fn legal_moves(&self) -> Vec<String> {
        let mut board = self.build_board();
        let mut storage = Vec::new();
        if let Some(mv) = board.can_make_road(&mut storage, None) {
            return vec![mv.to_ptn::<Board6>()];
        }
        storage.clear();
        generate_all_moves(&board, &mut storage);
        board
            .get_tak_threats(&storage, None)
            .into_iter()
            .map(|x| x.to_ptn::<Board6>())
            .collect()
    }
    pub fn user_play_move(&mut self, ptn_move: &str) -> Option<TinueResponse> {
        let mut board = self.build_board();
        let mut mv = GameMove::try_from_ptn(ptn_move, &board)?;
        match board.board().get(mv.dest_sq(6)).and_then(|x| x.top()) {
            Some(topaz_tak::Piece::WhiteWall) | Some(topaz_tak::Piece::BlackWall) => {
                mv = mv.set_crush();
            }
            _ => {}
        }
        if !board.legal_move(mv) {
            return None;
        }
        board.do_move(mv);
        if let Some(_end) = board.game_result() {
            if board.road(self.initial_side()) {
                return Some(TinueResponse::Road);
            } else {
                return Some(TinueResponse::NoThreats(None));
            }
        }
        // Check if a road is threatened
        let mut storage = Vec::new();
        board.null_move();
        if board.can_make_road(&mut storage, None).is_none() {
            return Some(TinueResponse::NoThreats(None));
        }
        board.rev_null_move();
        let pv_move = self.active_pv.get(0).map(|x| x.as_str()).unwrap_or("");
        if self.is_tinue && no_star(pv_move) == no_star(&mv.to_ptn::<Board6>()) {
            let reply = self
                .active_pv
                .get(1)
                .and_then(|x| GameMove::try_from_ptn(x, &board));
            self.active_pv.remove(0);
            if reply.is_some() {
                self.active_pv.remove(0);
            }
            return Some(TinueResponse::ExactResponse(reply));
        }
        // Fallback to search
        let mut search = TinueSearch::new(board)
            .quiet()
            .limit(250_000)
            .attacker(false);
        let tinue_res = search.is_tinue();
        self.is_tinue = tinue_res.unwrap_or(false);
        let pv = search.principal_variation();
        self.active_pv = pv
            .iter()
            .skip(1)
            .map(|x| no_star(&x.to_ptn::<Board6>()).to_string())
            .collect();
        let reply = pv.into_iter().next();
        match tinue_res {
            Some(true) => Some(TinueResponse::ValidResponse(reply)),
            Some(false) => Some(TinueResponse::PoorResponse(reply)),
            None => Some(TinueResponse::UnclearResponse(reply)),
        }
    }
    pub fn human_difficulty(&self) -> Difficulty {
        PUZZLES[self.puzzle_num].human_difficulty()
    }
}

pub fn no_star(ptn_move: &str) -> &str {
    ptn_move.trim_end_matches('*')
}

struct PuzzleData {
    puzzle_id: usize,
    game_id: usize,
    tps: String,
    pv: Vec<String>,
    difficulty: f32,
}

pub enum Difficulty {
    Easy = 0,
    Medium = 1,
    Hard = 2,
    Insane = 3,
}

impl std::fmt::Display for Difficulty {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Difficulty::Easy => "Easy",
            Difficulty::Medium => "Medium",
            Difficulty::Hard => "Hard",
            Difficulty::Insane => "Insane",
        };
        write!(f, "{}", s)
    }
}

impl PuzzleData {
    fn human_difficulty(&self) -> Difficulty {
        if self.difficulty < 8.0 {
            Difficulty::Easy
        } else if self.difficulty < 12.0 {
            Difficulty::Medium
        } else if self.difficulty < 16.0 {
            Difficulty::Hard
        } else {
            Difficulty::Insane
        }
    }
}
// Guess cutoffs: <1, 2.20, 3.5, 4.0

// pub fn list_difficulties() {
//     for puzzle in PUZZLES.iter() {
//         if puzzle.difficulty >= 3.0 {
//             println!("{}: {}, {:?}", puzzle.difficulty, puzzle.tps, puzzle.pv);
//         }
//         // println!("{}", puzzle.difficulty);
//     }
// }
