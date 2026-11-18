use meridian_crypto::aes::Aes;
use meridian_crypto::argon2::{argon2id, blocks_required, Argon2Error};
use meridian_crypto::blake2b::{variable, Blake2bHasher};
use meridian_crypto::ed25519::{secret_to_public, sign, verify};
use meridian_crypto::sha256::hash as sha256;
use meridian_crypto::sha512::hash as sha512;
use meridian_crypto::xts::XtsAes;

fn clean_hex(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

fn hex_digit(b: u8) -> u8 {
    match b {
        b'0'..=b'9' => b - b'0',
        b'a'..=b'f' => b - b'a' + 10,
        b'A'..=b'F' => b - b'A' + 10,
        _ => panic!("carácter hexadecimal inválido"),
    }
}

fn hexn(s: &str) -> Vec<u8> {
    let s = clean_hex(s);
    assert_eq!(s.len() % 2, 0, "hex de longitud impar");
    let bytes = s.as_bytes();
    (0..s.len())
        .step_by(2)
        .map(|i| (hex_digit(bytes[i]) << 4) | hex_digit(bytes[i + 1]))
        .collect()
}

fn hex<const N: usize>(s: &str) -> [u8; N] {
    let v = hexn(s);
    assert_eq!(v.len(), N, "longitud de vector inesperada");
    let mut out = [0u8; N];
    out.copy_from_slice(&v);
    out
}

#[test]
fn sha_nist_fips_180_4() {
    assert_eq!(
        sha256(b""),
        hex::<32>("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
    );
    assert_eq!(
        sha256(b"abc"),
        hex::<32>("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
    );
    assert_eq!(
        sha256(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
        hex::<32>("248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1")
    );

    assert_eq!(
        sha512(b""),
        hex::<64>("cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e")
    );
    assert_eq!(
        sha512(b"abc"),
        hex::<64>("ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f")
    );
    assert_eq!(
        sha512(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
        hex::<64>("204a8fc6dda82f0a0ced7beb8e08a41657c16ef468b228a8279be331a703c33596fd15c13b1b07f9aa1d3bea57789ca031ad85c7a71dd70354ec631238ca3445")
    );
}

#[test]
fn sha_large_and_streaming_vectors() {
    let million_a = vec![b'a'; 1_000_000];
    assert_eq!(
        sha256(&million_a),
        hex::<32>("cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0")
    );
    assert_eq!(
        sha512(&million_a),
        hex::<64>("e718483d0ce769644e2e42c7bc15b4638e1f98b13b2044285632a803afa973ebde0ff244877ea60a4cb0432ce577c31beb009c5c2c49aa2e4eadb217ad8cc09b")
    );

    let mut hasher = meridian_crypto::sha256::Sha256::new();
    for chunk in million_a.chunks(997) {
        hasher.update(chunk);
    }
    let mut digest = [0u8; 32];
    hasher.finalize(&mut digest);
    assert_eq!(digest, sha256(&million_a));
}

#[test]
fn blake2b_rfc_7693_selftest() {
    let md_lens = [20usize, 32, 48, 64];
    let in_lens = [0usize, 3, 128, 129, 255, 1024];
    let mut grand = Blake2bHasher::new(32);

    for &out_len in &md_lens {
        for &in_len in &in_lens {
            let data = selftest_seq(in_len, in_len as u32);

            let mut md = vec![0u8; out_len];
            let mut hasher = Blake2bHasher::new(out_len);
            hasher.update(&data);
            hasher.finalize(&mut md).unwrap();
            grand.update(&md);

            let key = selftest_seq(out_len, out_len as u32);
            let mut md_keyed = vec![0u8; out_len];
            let mut keyed = Blake2bHasher::with_key(out_len, &key);
            keyed.update(&data);
            keyed.finalize(&mut md_keyed).unwrap();
            grand.update(&md_keyed);
        }
    }

    let mut result = [0u8; 32];
    grand.finalize(&mut result).unwrap();
    assert_eq!(
        result,
        hex::<32>("c23a7800d98123bd10f506c61e29da5603d763b8bbad2e737f5e765a7bccd475"),
        "grand hash del selftest BLAKE2b del RFC 7693"
    );
}

fn selftest_seq(len: usize, seed: u32) -> Vec<u8> {
    let mut out = vec![0u8; len];
    let mut a = 0xDEAD4BADu32.wrapping_mul(seed);
    let mut b = 1u32;
    for byte in &mut out {
        let t = a.wrapping_add(b);
        a = b;
        b = t;
        *byte = (t >> 24) as u8;
    }
    out
}

#[test]
fn blake2b_variable_rfc_9106_h_prime() {
    let data = b"meridian phase 1 crypto base";

    let mut out32 = [0u8; 32];
    variable(32, data, &mut out32).unwrap();
    assert_eq!(
        out32,
        hex::<32>("f3082262434d71e500360f2d35b6f6ca154331cbb696c1fa026660b477bb25e9")
    );

    let mut out65 = [0u8; 65];
    variable(65, data, &mut out65).unwrap();
    assert_eq!(
        out65,
        hex::<65>("62222e825cd736d562ba7afe3840e31a680ae35dec89acaa473a271237ddb5b02acb66ea17dbd0c3866fc049ec5f570b431b3256114b0ba8fad322ce6ffe7b3245")
    );
}

#[test]
fn aes_fips_197_vectors() {
    let plaintext = hex::<16>("00112233445566778899aabbccddeeff");
    let cases = [
        (
            "000102030405060708090a0b0c0d0e0f",
            "69c4e0d86a7b0430d8cdb78070b4c55a",
        ),
        (
            "000102030405060708090a0b0c0d0e0f1011121314151617",
            "dda97ca4864cdfe06eaf70a0ec0d7191",
        ),
        (
            "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
            "8ea2b7ca516745bfeafc49904b496089",
        ),
    ];

    for (key_hex, expected_hex) in cases {
        let aes = Aes::new(&hexn(key_hex)).unwrap();
        let expected = hex::<16>(expected_hex);
        let ciphertext = aes.encrypt_block(&plaintext);
        assert_eq!(ciphertext, expected);
        assert_eq!(aes.decrypt_block(&ciphertext), plaintext);
    }
}

#[test]
fn xts_nist_sp_800_38e_vectors() {
    let vectors = [
        // XTSTestVectors.zip, XTSGenAES128.rsp, 128-hex tweak format, COUNT 1/301/101.
        // COUNT 201 (130 bits) se omite: no es byte-alineado y esta API opera con bytes.
        (
            "a1b90cba3f06ac353b2c343876081762090923026e91771815f29dab01932f2f",
            "4faef7117cda59c66e4b92013e768ad5",
            "ebabce95b14d3c8d6fb350390790311c",
            "778ae8b43cb98d5a825081d5be471c63",
        ),
        (
            "394c97881abd989d29c703e48a72b397a7acf51b59649eeea9b33274d8541df4",
            "4b15c684a152d485fe9937d39b168c29",
            "2f3b9dcfbae729583b1d1ffdd16bb6fe2757329435662a78f0",
            "f3473802e38a3ffef4d4fb8e6aa266ebde553a64528a06463e",
        ),
        (
            "b7b93f516aef295eff3a29d837cf1f135347e8a21dae616ff5062b2e8d78ce5e",
            "873edea653b643bd8bcf51403197ed14",
            "236f8a5b58dd55f6194ed70c4ac1a17f1fe60ec9a6c454d087ccb77d6b638c47",
            "22e6a3c6379dcf7599b052b5a749c7f78ad8a11b9f1aa9430cf3aef445682e19",
        ),
        // XTSTestVectors.zip, XTSGenAES256.rsp, 128-hex tweak format, COUNT 1/101.
        (
            "1ea661c58d943a0e4801e42f4b0947149e7f9f8e3e68d0c7505210bd311a0e7cd6e13ffdf2418d8d1911c004cda58da3d619b7e2b9141e58318eea392cf41b08",
            "adf8d92627464ad2f0428e84a9f87564",
            "2eedea52cd8215e1acc647e810bbc3642e87287f8d2e57e36c0a24fbc12a202e",
            "cbaad0e2f6cea3f50b37f934d46a9b130b9d54f07e34f36af793e86f73c6d7db",
        ),
        (
            "266c336b3b01489f3267f52835fd92f674374b88b4e1ebd2d36a5f457581d9d042c3eef7b0b7e5137b086496b4d9e6ac658d7196a23f23f036172fdb8faee527",
            "06b209a7a22f486ecbfadb0f3137ba42",
            "ca7d65ef8d3dfad345b61ccddca1ad81de830b9e86c7b426d76cb7db766852d981c6b21409399d78f42cc0b33a7bbb06",
            "c73256870cc2f4dd57acc74b5456dbd776912a128bc1f77d72cdebbf270044b7a43ceed29025e1e8be211fa3c3ed002d",
        ),
    ];

    for (key_hex, tweak_hex, pt_hex, ct_hex) in vectors {
        let cipher = XtsAes::new(&hexn(key_hex)).unwrap();
        let tweak = hex::<16>(tweak_hex);
        let expected_ct = hexn(ct_hex);
        let expected_pt = hexn(pt_hex);
        let mut data = expected_pt.clone();

        cipher.encrypt(&tweak, &mut data).unwrap();
        assert_eq!(data, expected_ct);
        cipher.decrypt(&tweak, &mut data).unwrap();
        assert_eq!(data, expected_pt);
    }
}

#[test]
fn argon2id_rfc_9106_vector() {
    assert_eq!(blocks_required(32, 4), Some(32));

    let blocks = blocks_required(32, 4).unwrap();
    let mut workspace = vec![0u8; blocks * 1024];
    let mut tag = [0u8; 32];

    argon2id(
        &[1u8; 32],
        &[2u8; 16],
        &[3u8; 8],
        &[4u8; 12],
        3,
        4,
        32,
        &mut workspace,
        &mut tag,
    )
    .unwrap();

    assert_eq!(
        tag,
        hex::<32>("0d640df58d78766c08c037a34a8b53c9d01ef0452d75b65eb52520e96b01e659")
    );

    assert_eq!(
        argon2id(
            &[1u8; 32],
            &[2u8; 16],
            &[],
            &[],
            3,
            4,
            32,
            &mut workspace[..(blocks * 1024) - 1],
            &mut tag
        ),
        Err(Argon2Error::WorkspaceTooSmall)
    );
    assert_eq!(
        argon2id(
            &[1u8; 32],
            &[2u8; 16],
            &[],
            &[],
            0,
            4,
            32,
            &mut workspace,
            &mut tag
        ),
        Err(Argon2Error::InvalidParameters)
    );
}

struct Ed25519Vector {
    name: &'static str,
    secret: &'static str,
    public: &'static str,
    message: &'static str,
    signature: &'static str,
}

#[test]
fn ed25519_rfc_8032_vectors() {
    let vectors = [
        Ed25519Vector {
            name: "TEST 1",
            secret: "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60",
            public: "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a",
            message: "",
            signature: "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b",
        },
        Ed25519Vector {
            name: "TEST 2",
            secret: "4ccd089b28ff96da9db6c346ec114e0f5b8a319f35aba624da8cf6ed4fb8a6fb",
            public: "3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c",
            message: "72",
            signature: "92a009a9f0d4cab8720e820b5f642540a2b27b5416503f8fb3762223ebdb69da085ac1e43e15996e458f3613d0f11d8c387b2eaeb4302aeeb00d291612bb0c00",
        },
        Ed25519Vector {
            name: "TEST 3",
            secret: "c5aa8df43f9f837bedb7442f31dcb7b166d38535076f094b85ce3a2e0b4458f7",
            public: "fc51cd8e6218a1a38da47ed00230f0580816ed13ba3303ac5deb911548908025",
            message: "af82",
            signature: "6291d657deec24024827e69c3abe01a30ce548a284743a445e3680d7db5ac3ac18ff9b538d16f290ae67f760984dc6594a7c15e9716ed28dc027beceea1ec40a",
        },
        Ed25519Vector {
            name: "TEST 1024",
            secret: "f5e5767cf153319517630f226876b86c8160cc583bc013744c6bf255f5cc0ee5",
            public: "278117fc144c72340f67d0f2316e8386ceffbf2b2428c9c51fef7c597f1d426e",
            message: "08b8b2b733424243760fe426a4b54908632110a66c2f6591eabd3345e3e4eb98fa6e264bf09efe12ee50f8f54e9f77b1e355f6c50544e23fb1433ddf73be84d879de7c0046dc4996d9e773f4bc9efe5738829adb26c81b37c93a1b270b20329d658675fc6ea534e0810a4432826bf58c941efb65d57a338bbd2e26640f89ffbc1a858efcb8550ee3a5e1998bd177e93a7363c344fe6b199ee5d02e82d522c4feba15452f80288a821a579116ec6dad2b3b310da903401aa62100ab5d1a36553e06203b33890cc9b832f79ef80560ccb9a39ce767967ed628c6ad573cb116dbefefd75499da96bd68a8a97b928a8bbc103b6621fcde2beca1231d206be6cd9ec7aff6f6c94fcd7204ed3455c68c83f4a41da4af2b74ef5c53f1d8ac70bdcb7ed185ce81bd84359d44254d95629e9855a94a7c1958d1f8ada5d0532ed8a5aa3fb2d17ba70eb6248e594e1a2297acbbb39d502f1a8c6eb6f1ce22b3de1a1f40cc24554119a831a9aad6079cad88425de6bde1a9187ebb6092cf67bf2b13fd65f27088d78b7e883c8759d2c4f5c65adb7553878ad575f9fad878e80a0c9ba63bcbcc2732e69485bbc9c90bfbd62481d9089beccf80cfe2df16a2cf65bd92dd597b0707e0917af48bbb75fed413d238f5555a7a569d80c3414a8d0859dc65a46128bab27af87a71314f318c782b23ebfe808b82b0ce26401d2e22f04d83d1255dc51addd3b75a2b1ae0784504df543af8969be3ea7082ff7fc9888c144da2af58429ec96031dbcad3dad9af0dcbaaaf268cb8fcffead94f3c7ca495e056a9b47acdb751fb73e666c6c655ade8297297d07ad1ba5e43f1bca32301651339e22904cc8c42f58c30c04aafdb038dda0847dd988dcda6f3bfd15c4b4c4525004aa06eeff8ca61783aacec57fb3d1f92b0fe2fd1a85f6724517b65e614ad6808d6f6ee34dff7310fdc82aebfd904b01e1dc54b2927094b2db68d6f903b68401adebf5a7e08d78ff4ef5d63653a65040cf9bfd4aca7984a74d37145986780fc0b16ac451649de6188a7dbdf191f64b5fc5e2ab47b57f7f7276cd419c17a3ca8e1b939ae49e488acba6b965610b5480109c8b17b80e1b7b750dfc7598d5d5011fd2dcc5600a32ef5b52a1ecc820e308aa342721aac0943bf6686b64b2579376504ccc493d97e6aed3fb0f9cd71a43dd497f01f17c0e2cb3797aa2a2f256656168e6c496afc5fb93246f6b1116398a346f1a641f3b041e989f7914f90cc2c7fff357876e506b50d334ba77c225bc307ba537152f3f1610e4eafe595f6d9d90d11faa933a15ef1369546868a7f3a45a96768d40fd9d03412c091c6315cf4fde7cb68606937380db2eaaa707b4c4185c32eddcdd306705e4dc1ffc872eeee475a64dfac86aba41c0618983f8741c5ef68d3a101e8a3b8cac60c905c15fc910840b94c00a0b9d0",
            signature: "0aab4c900501b3e24d7cdf4663326a3a87df5e4843b2cbdb67cbf6e460fec350aa5371b1508f9f4528ecea23c436d94b5e8fcd4f681e30a6ac00a9704a188a03",
        },
        Ed25519Vector {
            name: "TEST SHA(abc)",
            secret: "833fe62409237b9d62ec77587520911e9a759cec1d19755b7da901b96dca3d42",
            public: "ec172b93ad5e563bf4932c70e1245034c35467ef2efd4d64ebf819683467e2bf",
            message: "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f",
            signature: "dc2a4459e7369633a52b1bf277839a00201009a3efbf3ecb69bea2186c26b58909351fc9ac90b3ecfdfbc7c66431e0303dca179c138ac17ad9bef1177331a704",
        },
    ];

    for vector in vectors {
        let secret = hex::<32>(vector.secret);
        let public = hex::<32>(vector.public);
        let message = hexn(vector.message);
        let signature = hex::<64>(vector.signature);

        assert_eq!(secret_to_public(&secret), public, "{} pub", vector.name);
        assert_eq!(sign(&secret, &message), signature, "{} sign", vector.name);
        assert!(
            verify(&public, &message, &signature),
            "{} verify",
            vector.name
        );

        let mut tampered = signature;
        tampered[0] ^= 0x01;
        assert!(
            !verify(&public, &message, &tampered),
            "{} reject",
            vector.name
        );
    }
}
