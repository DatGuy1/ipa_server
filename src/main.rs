#[macro_use]
extern crate lazy_static;

#[macro_use]
extern crate rocket;

use std::collections::HashMap;

use rand::seq::SliceRandom;
use rand::SeedableRng;

use aws_config;
use aws_sdk_polly::{Client, Region};
use aws_sdk_polly::model::{Engine, LanguageCode, OutputFormat, TextType, VoiceId};
use rocket::response::status;
use rocket::response::stream::ReaderStream;
use rocket::State;
use rocket::serde::Deserialize;
use rocket::serde::json::Json;
use rocket_governor::{Method, Quota, ReqState, RocketGovernable, RocketGovernor};
use rocket_validation::{Validate, Validated};

mod cors;

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
        Quota::per_hour(Self::nonzero(100u32))
    }

    fn limit_info_allow(_method: Option<Method>, _route_name: Option<&str>, _state: &ReqState) -> bool {
        true
    }
}

#[derive(Debug, Deserialize, Validate)]
#[serde(crate = "rocket::serde")]
pub struct RequestData {
    #[validate(length(min = 1, max = 50))]
    ipa: String,
    language: String,
}

struct Polly {
    client: Client,
    speakers: HashMap<String, Vec<VoiceId>>,
}

#[post("/", format = "json", data = "<validated_data>")]
async fn speak(validated_data: Validated<Json<RequestData>>, polly: &State<Polly>, _limitguard: RocketGovernor<'_, RateLimitGuard>) -> Result<ReaderStream![impl rocket::tokio::io::AsyncRead], status::BadRequest<String>> {
    let data = validated_data.into_inner();
    let target_language = &*data.language;
    if !LANGUAGE_TO_CODE.contains_key(target_language) {
        return Err(status::BadRequest(Some(format!("Language {target_language} is unsupported"))));
    }

    let mut rng = rand::rngs::StdRng::from_entropy();

    let generic_language = &*generic_language_from_code(LANGUAGE_TO_CODE.get(target_language).unwrap().clone());
    if !polly.speakers.contains_key(generic_language) {
        return Err(status::BadRequest(Some(format!("Language {target_language} is unsupported"))));
    }

    let random_speaker = polly.speakers.get(generic_language).unwrap().choose(&mut rng).unwrap();
    let ssml_text = format!("<phoneme alphabet='ipa' ph='{}'></phoneme>", data.ipa);

    let resp = polly.client
        .synthesize_speech()
        .output_format(OutputFormat::OggVorbis)
        .text(ssml_text)
        .text_type(TextType::Ssml)
        .voice_id(random_speaker.clone())
        .send()
        .await
        .expect("failed to synthesize speech");

    Ok(ReaderStream::one(resp.audio_stream.into_async_read()))
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
async fn main() {
    let shared_config = aws_config::from_env().region(Region::new("eu-west-2")).load().await;
    let polly_client = Client::new(&shared_config);

    let mut all_voices: HashMap<String, Vec<VoiceId>> = HashMap::new();

    let voices_result = polly_client.describe_voices().send().await.expect("Please (re)initialise your AWS credentials. See https://docs.aws.amazon.com/cli/latest/userguide/cli-configure-files.html");
    for voice in voices_result.voices.unwrap() {
        if !voice.clone().supported_engines.unwrap().contains(&Engine::Standard) {
            continue;
        }

        let main_language = voice.language_code().unwrap().clone();
        let mut voice_languages: Vec<LanguageCode> = Vec::from(voice.additional_language_codes().unwrap_or_default());
        voice_languages.push(main_language);

        // println!("Language for {}: {} ({:#?}). Additional: {:#?}", voice.name().unwrap(), voice.language_name().unwrap(), voice.language_code().unwrap(), voice.additional_language_codes().unwrap_or_default());

        for voice_language in voice_languages {
            // Convert to generic language code by taking first two characters.
            // I hate it but what can you do.
            let generic_language = generic_language_from_code(voice_language).to_string();
            // println!("{} speaks {}", voice.name().unwrap(), generic_language);
            all_voices.entry(generic_language).or_insert(Vec::new()).push(voice.id().unwrap().clone());
        }
    }

    let polly = Polly {
        client: polly_client,
        speakers: all_voices,
    };

    let _ = rocket::build()
        .attach(cors::CORS)
        .attach(rocket_governor::LimitHeaderGen::default())
        .manage(polly)
        .mount("/", routes![index, speak, all_options])
        .register("/", catchers![rocket_validation::validation_catcher])
        .launch()
        .await;
}