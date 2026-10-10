use std::collections::VecDeque;

pub struct VoiceActivity {
    detector: earshot::Detector,
    frame: Vec<i16>,
    preroll: VecDeque<i16>,
    utterance: Vec<i16>,
    onset: usize,
    voiced: usize,
    silence: usize,
}

pub struct Activity {
    pub started: bool,
    pub voiced: bool,
    pub segment: Option<Vec<i16>>,
}

impl Default for VoiceActivity {
    fn default() -> Self {
        Self {
            detector: earshot::Detector::default(),
            frame: Vec::with_capacity(256),
            preroll: VecDeque::with_capacity(3200),
            utterance: Vec::new(),
            onset: 0,
            voiced: 0,
            silence: 0,
        }
    }
}

impl VoiceActivity {
    pub fn push(&mut self, samples: &[i16]) -> Vec<Activity> {
        let mut events = Vec::new();
        for &sample in samples {
            self.frame.push(sample);
            if self.frame.len() != 256 {
                continue;
            }
            let speech = self.detector.predict_i16(&self.frame) >= 0.5;
            let mut started = false;
            if self.utterance.is_empty() {
                self.preroll.extend(self.frame.iter().copied());
                while self.preroll.len() > 3200 {
                    self.preroll.pop_front();
                }
                self.onset = if speech { self.onset + 1 } else { 0 };
                if self.onset >= 3 {
                    self.utterance.extend(self.preroll.drain(..));
                    self.voiced = self.onset * 256;
                    self.silence = 0;
                    started = true;
                }
            } else {
                self.utterance.extend_from_slice(&self.frame);
                if speech {
                    self.voiced += 256;
                    self.silence = 0;
                } else {
                    self.silence += 256;
                }
            }
            let mut segment = None;
            if !self.utterance.is_empty()
                && (self.silence >= 10240 || self.utterance.len() >= 480000)
            {
                let audio = std::mem::take(&mut self.utterance);
                if self.voiced >= 2560 {
                    segment = Some(audio);
                }
                self.onset = 0;
                self.voiced = 0;
                self.silence = 0;
            }
            events.push(Activity {
                started,
                voiced: speech,
                segment,
            });
            self.frame.clear();
        }
        events
    }
}
