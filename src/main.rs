#[macro_use]
extern crate lazy_static;

#[macro_use]
extern crate rocket;

use std::collections::HashMap;

use log::{error, info};
use rand::SeedableRng;

use aws_config::{self, BehaviorVersion};
use aws_sdk_polly::types::{Engine, LanguageCode, OutputFormat, TextType, VoiceId};
use aws_sdk_polly::Client;
use rand::prelude::IndexedRandom;
use rocket::response::{status, stream::ReaderStream};
use rocket::serde::{json::Json, Deserialize};
use rocket::State;
use rocket_governor::{Method, Quota, ReqState, RocketGovernable, RocketGovernor};

mod cors;

const MAX_IPA_LENGTH: usize = 50;
const MIN_IPA_LENGTH: usize = 1;
const RATE_LIMIT_PER_HOUR: u32 = 100;

lazy_static! {
    // Wikipedia IPA language page to AWS LanguageCode
    static ref LANGUAGE_TO_CODE: HashMap<&'static str, LanguageCode> = HashMap::from([
        ("Arabic", LanguageCode::Arb),
        ("Catalan", LanguageCode::CaEs),
        ("Mandarin", LanguageCode::CmnCn),
        ("Welsh", LanguageCode::CyGb),
        ("Danish", LanguageCode::DaDk),
        ("Standard German", LanguageCode::DeAt),
        ("English", LanguageCode::EnUs),
        ("Spanish", LanguageCode::EsEs),
        ("French", LanguageCode::FrCa),
        ("Hindi and Urdu", LanguageCode::HiIn),
        ("Icelandic", LanguageCode::IsIs),
        ("Italian", LanguageCode::ItIt),
        ("Japanese", LanguageCode::JaJp),
        ("Korean", LanguageCode::KoKr),
        ("Norwegian", LanguageCode::NbNo),
        ("Dutch", LanguageCode::NlNl),
        ("Polish", LanguageCode::PlPl),
        ("Portuguese", LanguageCode::PtBr),
        ("Romanian", LanguageCode::RoRo),
        ("Russian", LanguageCode::RuRu),
        ("Swedish", LanguageCode::SvSe),
        ("Turkish", LanguageCode::TrTr)
    ]);
}

pub struct RateLimitGuard;

impl<'r> RocketGovernable<'r> for RateLimitGuard {
    fn quota(_method: Method, _route_name: &str) -> Quota {
        Quota::per_hour(Self::nonzero(RATE_LIMIT_PER_HOUR))
    }

    fn limit_info_allow(
        _method: Option<Method>,
        _route_name: Option<&str>,
        _state: &ReqState,
    ) -> bool {
        true
    }
}

#[derive(Debug, Deserialize)]
pub struct RequestData {
    ipa: String,
    language: String,
}

struct Polly {
    client: Client,
    speakers: HashMap<String, Vec<VoiceId>>,
}

impl Polly {
    async fn synthesize_speech(
        &self,
        ipa: &str,
        voice_id: &VoiceId,
    ) -> Result<impl rocket::tokio::io::AsyncRead, aws_sdk_polly::Error> {
        let ssml_text = format!("<phoneme alphabet='ipa' ph='{}'></phoneme>", ipa);

        let resp = self
            .client
            .synthesize_speech()
            .output_format(OutputFormat::OggVorbis)
            .text(ssml_text)
            .text_type(TextType::Ssml)
            .voice_id(voice_id.clone())
            .send()
            .await?;

        Ok(resp.audio_stream.into_async_read())
    }
}

#[post("/", format = "json", data = "<validated_data>")]
async fn speak(
    validated_data: Json<RequestData>,
    polly: &State<Polly>,
    _limitguard: RocketGovernor<'_, RateLimitGuard>,
) -> Result<ReaderStream![impl rocket::tokio::io::AsyncRead], status::BadRequest<String>> {
    let data = validated_data.into_inner();

    // Validate IPA length
    let ipa_length = data.ipa.len();
    if ipa_length < MIN_IPA_LENGTH || ipa_length > MAX_IPA_LENGTH {
        return Err(status::BadRequest(
            format!("IPA must be between {} and {} characters", MIN_IPA_LENGTH, MAX_IPA_LENGTH),
        ));
    }

    // Validate language
    let target_language = &*data.language;
    let language_code = LANGUAGE_TO_CODE
        .get(target_language)
        .ok_or_else(|| status::BadRequest(format!("Language {target_language} is unsupported")))?;

    let generic_language = generic_language_from_code(language_code.clone());
    let speakers = polly.speakers.get(&generic_language).ok_or_else(|| {
        status::BadRequest(format!(
            "No speakers available for language {target_language}"
        ))
    })?;

    // Select random speaker
    let mut rng = rand::rngs::StdRng::from_rng(&mut rand::rng());
    let random_speaker = speakers
        .choose(&mut rng)
        .ok_or_else(|| status::BadRequest("No available speakers".to_string()))?;

    info!(
        "Synthesizing speech for IPA: {}, language: {}, speaker: {}",
        data.ipa, target_language, random_speaker
    );

    // Synthesize speech
    match polly.synthesize_speech(&data.ipa, random_speaker).await {
        Ok(audio_stream) => Ok(ReaderStream::one(audio_stream)),
        Err(e) => {
            error!("Failed to synthesize speech: {}", e);
            Err(status::BadRequest(format!(
                "Failed to synthesize speech: {}",
                e.to_string()
            )))
        }
    }
}

#[get("/")]
fn index() -> &'static str {
    "This is a ipa_server, running on Rocket (Rust). You probably meant to do a POST request"
}

#[options("/<_..>")]
fn all_options() {}

fn generic_language_from_code(master_code: LanguageCode) -> String {
    master_code.as_str().get(0..2).unwrap().to_string()
}

#[rocket::main]
async fn main() -> Result<(), rocket::Error> {
    // Initialize logging
    env_logger::init();
    info!("Starting IPA server...");

    let shared_config = aws_config::defaults(BehaviorVersion::latest())
        .region("eu-west-2")
        .load()
        .await;
    let polly_client = Client::new(&shared_config);

    let mut all_voices: HashMap<String, Vec<VoiceId>> = HashMap::new();

    let voices_result = polly_client
        .describe_voices()
        .send()
        .await
        .expect("Failed to describe voices - please check AWS credentials");

    for voice in voices_result.voices.unwrap_or_default() {
        if !voice
            .clone()
            .supported_engines
            .unwrap_or_default()
            .contains(&Engine::Standard)
        {
            continue;
        }

        let main_language = voice.language_code().unwrap().clone();
        let mut voice_languages: Vec<LanguageCode> = voice.additional_language_codes().to_vec();
        voice_languages.push(main_language);

        // info!("Language for {}: {} ({:#?}). Additional: {:#?}", voice.name().unwrap(), voice.language_name().unwrap(), voice.language_code().unwrap(), voice.additional_language_codes().unwrap_or_default());
        for voice_language in voice_languages {
            let generic_language = generic_language_from_code(voice_language);
            if let Some(voice_id) = voice.id() {
                all_voices
                    .entry(generic_language)
                    .or_insert_with(Vec::new)
                    .push(voice_id.clone());
            }
        }
    }

    info!("Loaded {} voice languages", all_voices.len());

    let polly = Polly {
        client: polly_client,
        speakers: all_voices,
    };

    let _ = rocket::build()
        .attach(cors::CORS)
        .attach(rocket_governor::LimitHeaderGen::default())
        .manage(polly)
        .mount("/", routes![index, speak, all_options])
        .launch()
        .await?;

    Ok(())
}
