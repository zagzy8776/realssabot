use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Write};
use rand::Rng;
use serde::{Deserialize, Serialize};

// ─────────────────────────────────────────────────────────────────────────────
// Config
// ─────────────────────────────────────────────────────────────────────────────
#[derive(Clone, Serialize, Deserialize)]
pub struct TransformerConfig {
    pub vocab_size: usize,
    pub embed_dim: usize,
    pub hidden_dim: usize,
    pub seq_len: usize,
    pub num_layers: usize,
}

impl Default for TransformerConfig {
    fn default() -> Self {
        Self {
            vocab_size: 4000,
            embed_dim: 128,
            hidden_dim: 512,
            seq_len: 64,
            num_layers: 4,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// BPE Tokenizer
// ─────────────────────────────────────────────────────────────────────────────
#[derive(Clone, Serialize, Deserialize)]
pub struct BpeTokenizer {
    pub merges: HashMap<(usize, usize), usize>,
    pub vocab: HashMap<usize, Vec<u8>>,
}

impl BpeTokenizer {
    pub fn new() -> Self {
        let mut vocab = HashMap::new();
        for i in 0..256 {
            vocab.insert(i, vec![i as u8]);
        }
        Self {
            merges: HashMap::new(),
            vocab,
        }
    }

    pub fn encode(&self, text: &str) -> Vec<usize> {
        let mut ids: Vec<usize> = text.as_bytes().iter().map(|&b| b as usize).collect();
        while ids.len() >= 2 {
            let mut best_pair: Option<(usize, usize)> = None;
            let mut best_idx = usize::MAX;

            for i in 0..ids.len() - 1 {
                let pair = (ids[i], ids[i + 1]);
                if let Some(&idx) = self.merges.get(&pair) {
                    if idx < best_idx {
                        best_idx = idx;
                        best_pair = Some(pair);
                    }
                }
            }

            let pair = match best_pair {
                Some(p) => p,
                None => break,
            };

            let mut new_ids = Vec::with_capacity(ids.len());
            let mut i = 0;
            while i < ids.len() {
                if i < ids.len() - 1 && ids[i] == pair.0 && ids[i + 1] == pair.1 {
                    new_ids.push(best_idx);
                    i += 2;
                } else {
                    new_ids.push(ids[i]);
                    i += 1;
                }
            }
            ids = new_ids;
        }
        ids
    }

    pub fn decode(&self, ids: &[usize]) -> String {
        let mut bytes = Vec::new();
        for &id in ids {
            if let Some(b) = self.vocab.get(&id) {
                bytes.extend_from_slice(b);
            }
        }
        String::from_utf8_lossy(&bytes).to_string()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Single Transformer Layer weights
// ─────────────────────────────────────────────────────────────────────────────
#[derive(Clone, Serialize, Deserialize)]
pub struct TransformerLayerWeights {
    pub ln1_weight: Vec<f32>,
    pub ln1_bias: Vec<f32>,
    pub wq: Vec<f32>,
    pub wk: Vec<f32>,
    pub wv: Vec<f32>,
    pub wo: Vec<f32>,
    pub ln2_weight: Vec<f32>,
    pub ln2_bias: Vec<f32>,
    pub w1: Vec<f32>,
    pub w2: Vec<f32>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Complete Transformer Weights
// ─────────────────────────────────────────────────────────────────────────────
#[derive(Serialize, Deserialize)]
pub struct TransformerWeights {
    pub config: TransformerConfig,
    pub token_embeddings: Vec<f32>,
    pub pos_embeddings: Vec<f32>,
    pub layers: Vec<TransformerLayerWeights>,
    pub ln_final_weight: Vec<f32>,
    pub ln_final_bias: Vec<f32>,
    pub output_proj: Vec<f32>,
    pub tokenizer: BpeTokenizer,
}

impl TransformerWeights {
    pub fn new(config: TransformerConfig) -> Self {
        let mut rng = rand::thread_rng();
        let d = config.embed_dim;
        let h = config.hidden_dim;

        let mut rand_vec = |size: usize| -> Vec<f32> {
            (0..size).map(|_| rng.gen_range(-0.02..0.02)).collect()
        };

        let mut layers = Vec::new();
        for _ in 0..config.num_layers {
            layers.push(TransformerLayerWeights {
                ln1_weight: vec![1.0; d],
                ln1_bias: vec![0.0; d],
                wq: rand_vec(d * d),
                wk: rand_vec(d * d),
                wv: rand_vec(d * d),
                wo: rand_vec(d * d),
                ln2_weight: vec![1.0; d],
                ln2_bias: vec![0.0; d],
                w1: rand_vec(d * h),
                w2: rand_vec(h * d),
            });
        }

        Self {
            token_embeddings: rand_vec(config.vocab_size * d),
            pos_embeddings: rand_vec(config.seq_len * d),
            layers,
            ln_final_weight: vec![1.0; d],
            ln_final_bias: vec![0.0; d],
            output_proj: rand_vec(config.vocab_size * d),
            tokenizer: BpeTokenizer::new(),
            config,
        }
    }

    pub fn load_from_file(path: &str) -> Result<Self, std::io::Error> {
        let mut file = File::open(path)?;
        let mut buf = Vec::new();
        file.read_to_end(&mut buf)?;
        let mut cursor = 0usize;

        if buf.len() < 4 || &buf[0..4] != b"RSA2" {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Invalid magic header in brain_weights.bin (expected RSA2)",
            ));
        }
        cursor += 4;

        let read_i32 = |cursor: &mut usize| -> usize {
            let val = i32::from_le_bytes(buf[*cursor..*cursor + 4].try_into().unwrap()) as usize;
            *cursor += 4;
            val
        };

        let read_floats = |cursor: &mut usize, count: usize| -> Vec<f32> {
            let byte_len = count * 4;
            let slice = &buf[*cursor..*cursor + byte_len];
            let floats: Vec<f32> = slice
                .chunks_exact(4)
                .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
                .collect();
            *cursor += byte_len;
            floats
        };

        let vocab_size = read_i32(&mut cursor);
        let embed_dim = read_i32(&mut cursor);
        let hidden_dim = read_i32(&mut cursor);
        let seq_len = read_i32(&mut cursor);
        let num_layers = read_i32(&mut cursor);

        let config = TransformerConfig {
            vocab_size,
            embed_dim,
            hidden_dim,
            seq_len,
            num_layers,
        };

        let token_embeddings = read_floats(&mut cursor, vocab_size * embed_dim);
        let pos_embeddings = vec![0.0f32; seq_len * embed_dim];

        let mut layers = Vec::with_capacity(num_layers);
        for _ in 0..num_layers {
            let ln1_weight = read_floats(&mut cursor, embed_dim);
            let ln1_bias = read_floats(&mut cursor, embed_dim);
            let ln2_weight = read_floats(&mut cursor, embed_dim);
            let ln2_bias = read_floats(&mut cursor, embed_dim);
            let wq = read_floats(&mut cursor, embed_dim * embed_dim);
            let wk = read_floats(&mut cursor, embed_dim * embed_dim);
            let wv = read_floats(&mut cursor, embed_dim * embed_dim);
            let wo = read_floats(&mut cursor, embed_dim * embed_dim);
            let w1 = read_floats(&mut cursor, hidden_dim * embed_dim);
            let w2 = read_floats(&mut cursor, embed_dim * hidden_dim);

            layers.push(TransformerLayerWeights {
                ln1_weight,
                ln1_bias,
                wq,
                wk,
                wv,
                wo,
                ln2_weight,
                ln2_bias,
                w1,
                w2,
            });
        }

        let ln_final_weight = read_floats(&mut cursor, embed_dim);
        let ln_final_bias = read_floats(&mut cursor, embed_dim);
        let output_proj = read_floats(&mut cursor, vocab_size * embed_dim);

        let num_merges = read_i32(&mut cursor);
        let mut tokenizer = BpeTokenizer::new();
        for _ in 0..num_merges {
            let p0 = read_i32(&mut cursor);
            let p1 = read_i32(&mut cursor);
            let idx = read_i32(&mut cursor);
            tokenizer.merges.insert((p0, p1), idx);

            let mut v = Vec::new();
            if let Some(b0) = tokenizer.vocab.get(&p0) {
                v.extend_from_slice(b0);
            }
            if let Some(b1) = tokenizer.vocab.get(&p1) {
                v.extend_from_slice(b1);
            }
            tokenizer.vocab.insert(idx, v);
        }

        Ok(Self {
            config,
            token_embeddings,
            pos_embeddings,
            layers,
            ln_final_weight,
            ln_final_bias,
            output_proj,
            tokenizer,
        })
    }

    pub fn save_to_file(&self, path: &str) -> Result<(), std::io::Error> {
        let buffer = bincode::serialize(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let mut file = File::create(path)?;
        file.write_all(&buffer)?;
        Ok(())
    }

    pub fn tokenize(&self, text: &str) -> Vec<usize> {
        self.tokenizer.encode(text)
    }

    pub fn detokenize(&self, ids: &[usize]) -> String {
        self.tokenizer.decode(ids)
    }

    pub fn update_vocabulary(&mut self, _sentences: &[String]) {}

    fn layer_norm(x: &[f32], gamma: &[f32], beta: &[f32]) -> Vec<f32> {
        let n = x.len();
        let mean: f32 = x.iter().sum::<f32>() / n as f32;
        let var: f32 = x.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / n as f32;
        let inv_std = 1.0 / (var + 1e-5_f32).sqrt();

        x.iter().enumerate()
            .map(|(i, &v)| (v - mean) * inv_std * gamma[i] + beta[i])
            .collect()
    }

    fn gelu(x: f32) -> f32 {
        x * (1.0 / (1.0 + (-1.702 * x).exp()))
    }

    pub fn forward(&self, tokens: &[usize]) -> Vec<f32> {
        let d = self.config.embed_dim;
        let seq_len = tokens.len();

        if seq_len == 0 {
            return vec![0.0; self.config.vocab_size];
        }

        let mut x = vec![0.0f32; seq_len * d];
        for i in 0..seq_len {
            let tok_id = tokens[i].min(self.config.vocab_size - 1);
            let pos_id = i.min(self.config.seq_len - 1);
            let tok_off = tok_id * d;
            let pos_off = pos_id * d;
            for j in 0..d {
                x[i * d + j] = self.token_embeddings[tok_off + j] + self.pos_embeddings[pos_off + j];
            }
        }

        for layer in &self.layers {
            let mut new_x = vec![0.0f32; seq_len * d];

            let mut normed = vec![0.0f32; seq_len * d];
            for i in 0..seq_len {
                let slice = &x[i * d..(i + 1) * d];
                let ln = Self::layer_norm(slice, &layer.ln1_weight, &layer.ln1_bias);
                normed[i * d..(i + 1) * d].copy_from_slice(&ln);
            }

            let mut q = vec![0.0f32; seq_len * d];
            let mut k = vec![0.0f32; seq_len * d];
            let mut v = vec![0.0f32; seq_len * d];
            for i in 0..seq_len {
                for j in 0..d {
                    let mut sq = 0.0f32;
                    let mut sk = 0.0f32;
                    let mut sv = 0.0f32;
                    for l in 0..d {
                        sq += normed[i * d + l] * layer.wq[l * d + j];
                        sk += normed[i * d + l] * layer.wk[l * d + j];
                        sv += normed[i * d + l] * layer.wv[l * d + j];
                    }
                    q[i * d + j] = sq;
                    k[i * d + j] = sk;
                    v[i * d + j] = sv;
                }
            }

            let scale = 1.0 / (d as f32).sqrt();
            let mut attn_out = vec![0.0f32; seq_len * d];
            for i in 0..seq_len {
                let mut scores = vec![0.0f32; seq_len];
                let mut max_score = -f32::INFINITY;
                for j in 0..seq_len {
                    let mut s = 0.0f32;
                    for l in 0..d {
                        s += q[i * d + l] * k[j * d + l];
                    }
                    scores[j] = s * scale;
                    if scores[j] > max_score { max_score = scores[j]; }
                }
                let mut exp_sum = 0.0f32;
                for j in 0..seq_len {
                    scores[j] = (scores[j] - max_score).exp();
                    exp_sum += scores[j];
                }
                for j in 0..seq_len {
                    scores[j] /= exp_sum;
                }
                for j in 0..d {
                    let mut sum = 0.0f32;
                    for l in 0..seq_len {
                        sum += scores[l] * v[l * d + j];
                    }
                    attn_out[i * d + j] = sum;
                }
            }

            let mut proj = vec![0.0f32; seq_len * d];
            for i in 0..seq_len {
                for j in 0..d {
                    let mut sum = 0.0f32;
                    for l in 0..d {
                        sum += attn_out[i * d + l] * layer.wo[l * d + j];
                    }
                    proj[i * d + j] = sum;
                }
            }

            for i in 0..seq_len * d {
                new_x[i] = x[i] + proj[i];
            }

            let mut normed2 = vec![0.0f32; seq_len * d];
            for i in 0..seq_len {
                let slice = &new_x[i * d..(i + 1) * d];
                let ln = Self::layer_norm(slice, &layer.ln2_weight, &layer.ln2_bias);
                normed2[i * d..(i + 1) * d].copy_from_slice(&ln);
            }

            let h = self.config.hidden_dim;
            for i in 0..seq_len {
                let mut hidden = vec![0.0f32; h];
                for j in 0..h {
                    let mut sum = 0.0f32;
                    for l in 0..d {
                        sum += normed2[i * d + l] * layer.w1[l * h + j];
                    }
                    hidden[j] = Self::gelu(sum);
                }
                for j in 0..d {
                    let mut sum = 0.0f32;
                    for l in 0..h {
                        sum += hidden[l] * layer.w2[l * d + j];
                    }
                    new_x[i * d + j] += sum;
                }
            }

            x = new_x;
        }

        let last_pos = (seq_len - 1) * d;
        let last_slice = &x[last_pos..last_pos + d];
        let final_normed = Self::layer_norm(last_slice, &self.ln_final_weight, &self.ln_final_bias);

        let mut logits = vec![0.0f32; self.config.vocab_size];
        for i in 0..self.config.vocab_size {
            let mut dot = 0.0f32;
            for j in 0..d {
                dot += final_normed[j] * self.output_proj[j * self.config.vocab_size + i];
            }
            logits[i] = dot;
        }

        logits
    }

    pub fn sample_next_token_id(&self, logits: &[f32], temperature: f32) -> usize {
        let mut max_logit = -f32::INFINITY;
        for &val in logits {
            if val > max_logit { max_logit = val; }
        }

        let mut exp_logits = vec![0.0f32; logits.len()];
        let mut sum = 0.0f32;
        for i in 0..logits.len() {
            let val = ((logits[i] - max_logit) / temperature).exp();
            exp_logits[i] = val;
            sum += val;
        }

        let mut rng = rand::thread_rng();
        let r: f32 = rng.gen();
        let mut cumulative = 0.0f32;

        for i in 0..exp_logits.len() {
            cumulative += exp_logits[i] / sum;
            if r <= cumulative {
                return i;
            }
        }

        0
    }

    pub fn sample_next_word(&self, logits: &[f32], temperature: f32) -> String {
        let id = self.sample_next_token_id(logits, temperature);
        self.tokenizer.decode(&[id])
    }

    pub fn train_step(&mut self, tokens: &[usize], learning_rate: f32) -> f32 {
        // Simplified online training placeholder
        let logits = self.forward(tokens);
        let mut loss = 0.0f32;
        if !tokens.is_empty() {
            let target = tokens[tokens.len() - 1].min(self.config.vocab_size - 1);
            let max_logit = logits.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
            let mut exp_sum = 0.0f32;
            for &l in &logits {
                exp_sum += (l - max_logit).exp();
            }
            let log_prob = logits[target] - max_logit - exp_sum.ln();
            loss = -log_prob;
            // Very light weight update (placeholder for real backprop)
            let _ = learning_rate;
        }
        loss
    }
}
