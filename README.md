# ipa_server

**ipa_server** is the backend for a small project I started to read out loud the IPA on Wikipedia articles.
It definitely isn't the greatest and definitely isn't the cleanest, but it does the job.

# Basic Installation
1. Install https://github.com/DatGuy1/Wikipedia-IPA-Extension
2. Set up your [AWS Credentials Configuration](https://docs.aws.amazon.com/cli/latest/userguide/cli-configure-files.html)
3. Clone this repository and run `cargo run`
4. Take the URL that Rocket gives you, such as `http://127.0.0:8000`, and put it in the extension's options
5. You're done!