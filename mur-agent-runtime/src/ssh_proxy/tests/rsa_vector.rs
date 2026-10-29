//! Fixed RSA-2048 vector: public modulus, e = 65537, and PKCS#1 v1.5
//! signatures over [`SESSION_ID`]. Generated once with LibreSSL
//! (`openssl genrsa 2048`, `openssl dgst -sha256|-sha512 -sign`); the private
//! key was discarded. `ring` cannot sign RSA without a key file, so the
//! vector is checked in.

pub const SESSION_ID: &[u8] = b"mur-ssh-proxy-test-session-id-32";

pub const N_HEX: &[&str] = &[
    "d284b134cf99972294133bfa37352d883c4dbc75959fd76e6e7278c07e2aa53f",
    "a9ae56a79fec18c044e0d8f4551a22072f69889bd036aef167bc5edb8aba32b5",
    "6fc713a044c8b5395113694e7a7b274e915f871ed22e8ea38fabffd8cb27d084",
    "51ccac03b488de10c0df452b303b13e1df67cb9ea7bac7f22bf6eb6534be5ea6",
    "af3beeaa577ea82c0bb13b81210ec0319bfc8ac033c1ec909a7cce85d7365c01",
    "6cb56b8339bd7e1cd79566e35910ea44dc7dfb5954defc44ac16bf68c83232cb",
    "bb0afb1fa453d3cc57bf0a2f68c2f1dcfc3ad441b967b6fdc8225d491001c19b",
    "82e1fbb685a1b2e97ffe884275393351414759aa962118ba6e5daf5ee0892095",
];

pub const SIG_SHA256_HEX: &[&str] = &[
    "c34c6b4bf3ae9a14feb9d77cad5742d82ad22bbd9e7aa3e9b775f3f3a996071c",
    "d253ead83c4f2662b122e040c3294981c6807e5ab5e1281b1f7fb184f3c160d1",
    "0fb048471723c05a5dcd16b9ffbe3e6dd757561efdfa9cd9600c11271179948f",
    "30f10fcb7130d611f1071ab8252ee042888a5585c5df565e4ee414360e398d5a",
    "8ea8d0b9d7023f242040a5c30081c0709e98ee17211ad71142e54f8db56b4e71",
    "d5b708112712144a449b32639b77feb89120b687f5712832f04695450ad0222e",
    "2c6618a8bc6c42082448c623df23aa9f0be1fabc105ca1e1a34c13c78dd769b6",
    "bbde0f523fe2b6c53106711bd5c160eef6d8b4e506231c30971b3a5e15038efd",
];

pub const SIG_SHA512_HEX: &[&str] = &[
    "08c4530ace50fe630c362b2939656786c5b38747a64307886481922cc2b02643",
    "cc4353c2f0525e1a95d1e3e37ef8271b2e7fbaa8ea1803dc8f915e74f13ddd20",
    "d6a56fd51bed3363f6723daeccca89b89fe197e910f814cfc2eb7926ce252e41",
    "5cc9ec922c822946dccfb752ed928cce0b050aaa3fd8e88b8508daa33d87a2d5",
    "8bba0d2c6563cf94c0c437e0f524162df43eae98623db6e0fe71e6d698ec6637",
    "92ee6cb86f10a4d720500b7b908a498fa02d7c02bb90ea849cfa56982d51c7af",
    "00e98d3b75038c1f473f299dfb4ce4bb7367924f154837bdc36880b2f5697c98",
    "364a196ae11b2f40b3543134a54c6bb3ae0151691e05fd255c994d83385d6f33",
];
