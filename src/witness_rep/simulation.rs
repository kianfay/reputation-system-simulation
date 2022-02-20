use crate::witness_rep::{
    iota_did::create_and_upload_did::{create_n_dids, Key, RunMode},
    transaction::{generate_contract, generate_sigs},
    transaction::{
        transaction::{transact, LazyMethod},
        participant::{ParticipantIdentity, IdInfo, ReliabilityMap}
    },
    utility::{verify_tx, read_msgs, extract_msgs},
};

use trust_score_generator::trust_score_generators::{
   trivial_tsg::tsg_organization,
   utility::parse_messages
};

use iota_streams::{
    app::transport::tangle::{TangleAddress, client::Client},
    app_channels::api::tangle::{
        Author, ChannelType, Subscriber
    },
    app_channels::Tangle,
    core::Result
};
use identity::{
    crypto::KeyPair,
};

use rand::Rng;
use std::collections::BTreeSet;
use std::collections::HashSet;
use std::collections::HashMap;
use std::convert::TryInto;
use std::iter::FromIterator;
use std::fs;

pub const ALPH9: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZ9";
pub const DEFAULT_DURATION: u32 = 60*60*24*365; // 1 year

// For now this simulation is capturing the abstract scenario where the initiating participant wishes 
// to informally buy something from somebody nearby. However, not all people around them are particpants
// of the system he uses. Therefore, the average_proximity paramater is included. This  represents the
// chance a participant being in range of some other participant.
// 
// Params:
//      - average_proximity: [0,1], 1 meaning all participants are in range
//      - witness_floor: the minimum number of witnesses in a transaction
//      - runs: the number of iterations of the simulations
//      - reliability: an array assigning a reliability score to participants at the respective indices
//      - organizations: an array assigning a organization to participants at the respective indices
pub async fn simulation(
    node_url: &str,
    num_participants: usize,
    average_proximity: f32,
    witness_floor: usize,
    runs: usize,
    reliability: Vec<f32>,
    organizations: Vec<usize>
) -> Result<()> {

    if reliability.len() != num_participants {
        panic!("Number of elements in 'reliability' parameter must equal the num_participants!");
    } else if reliability.len() != num_participants {
        panic!("Number of elements in 'organizations' parameter must equal the num_participants!");
    }


    //--------------------------------------------------------------
    //--------------------------------------------------------------
    //  CREATE ORGANIZATIONS WHICH ACT AS AN OVERLAY FOR PARTICIPANTS
    //--------------------------------------------------------------

    // we find the set of organizations
    let orgs_set: HashSet<&usize> = HashSet::from_iter(organizations.iter());
    let orgs: Vec<&usize> = orgs_set.into_iter().collect();

    // in their simplest form, an organization can be represented by
    // a keypair, so we assign one to each organization
    let org_did_details = create_n_dids(orgs.len(), RunMode::Testing).await?;

    // we create a mapping of organization index to public key
    let mut org_kp_map: HashMap<usize, KeyPair> = HashMap::new();
    let mut i = 0;
    for (_, (kp, _), _) in org_did_details {
        org_kp_map.insert(orgs[i].clone(), kp);
        i += 1;
    }

    //--------------------------------------------------------------
    // CREATE PARTICIPANTS FOR SIMULATION
    // (MORE DETAILS IN ALL_IN_ONE_TRANSACTION.RS)
    //--------------------------------------------------------------
    let client = Client::new_from_url(node_url);

    let seed: &str = &(0..81)
        .map(|_| {
            ALPH9
                .chars()
                .nth(rand::thread_rng().gen_range(0, 27))
                .unwrap()
        })
        .collect::<String>();

    let on_a: &mut Author<Tangle> = &mut Author::new(seed, ChannelType::MultiBranch, client.clone());
    
    // create Decentalised Ids (for now, none needed for the organization)
    let did_details = create_n_dids(num_participants, RunMode::Testing).await?;
    
    let part_did_secret : Vec<Key> = did_details
                                            .iter()
                                            .map(|(_, (_,(_, privkey)), _)| *privkey)
                                            .collect();
    
    let part_did_kps : Vec<&KeyPair> = did_details
                                            .iter()
                                            .map(|(_, (kp,_), _)| kp)
                                            .collect();

    // create channel subscriber instances
    let participants: &mut Vec<ParticipantIdentity> = &mut Vec::new();
    for i in 0..num_participants{
        let name = format!("Participant {}", i);
        let tn = Subscriber::new(&name, client.clone());
        let org_kp = &org_kp_map[&organizations[i]];
        let part_did_pk = generate_sigs::get_multibase(&part_did_kps[i]);
        let reliability_map: ReliabilityMap = HashMap::new();

        let id = ParticipantIdentity {
            channel_client: tn,
            id_info: IdInfo {
                did_key: part_did_secret[i],
                reliability: reliability[i],
                org_cert: generate_sigs::generate_org_cert(part_did_pk, org_kp, DEFAULT_DURATION)?
            },
            reliability_map: reliability_map
        };
        participants.push(id);
    }
    
    //--------------------------------------------------------------
    // RUN SIMULATION
    //--------------------------------------------------------------

    // author creates the channel 
    println!("Creating the channel:");
    let announcement_link = on_a.send_announce().await?;
    let ann_link_string = announcement_link.to_string();
    println!(
        "-- Announcement Link: {} Tangle Index: {:#}\n",
        ann_link_string, announcement_link.to_msg_index()
    );

    // generate the lazy methods (currenlty the first half of the runs are 
    // 'constant true' and the second half are 'random')
    println!("Generating lazy methods:");
    let lazy_methods: Vec<LazyMethod> = (0..=runs)
        .map(|x| {
            if x as f32 >= (runs as f32)/2.0 {
                LazyMethod::Constant(true)
            } else {
                LazyMethod::Random
            }
        }).collect::<Vec<LazyMethod>>()
        .try_into().expect("wrong size iterator");
    println!("-- Lazy methods to be used: {:?}\n", lazy_methods);

    for i in 0..runs {
        // run the iteration
        simulation_iteration(
            on_a,
            participants,
            average_proximity,
            witness_floor,
            lazy_methods[i].clone(),
            announcement_link
        ).await?;

        // participants update their reliability scores of each other
        let channel_msgs = read_msgs::read_msgs(node_url, &ann_link_string, seed).await?;
        let branch_msgs = extract_msgs::extract_msg(channel_msgs, i);
        let parsed_msgs = parse_messages::parse_messages(&branch_msgs[0])?;
        for part in participants.into_iter() {
            let (tn_verdicts, wn_verdicts) = tsg_organization(
                parsed_msgs.clone(),
                part.id_info.org_cert.org_pubkey.clone(),
                0.5
            );
            println!("tn_verdicts: {:?}", tn_verdicts);
            println!("wn_verdicts: {:?}\n", wn_verdicts);
        }
    }

    // verify the transaction
    let branches = verify_tx::WhichBranch::FromBranch(0);
    let channel_msgs = read_msgs::read_msgs(node_url, &ann_link_string, seed).await?;
    let (verified, msgs, pks) = verify_tx::verify_txs(channel_msgs, branches).await?;

    if !verified {
        panic!("One of the messages could not be verified");
    }

    // for each message
    let mut output: String = String::new();
    for i in 0..msgs.len() {
        // print the message and then the id_info of the sender
        let msg = format!("Message {:?}\n", msgs[i]);
        let pk = format!("Channel pubkey: {:?}\n\n", pks[i]);
        output.push_str(&msg);
        output.push_str(&pk);
    }
    fs::write("output.txt", output).expect("Unable to write file");
    
    return Ok(());
}


// Runs a single iteration of a simualtion
pub async fn simulation_iteration(
    mut on_a: &mut Author<Tangle>,
    mut participants: &mut Vec<ParticipantIdentity>,
    average_proximity: f32,
    witness_floor: usize,
    lazy_method: LazyMethod,
    announcement_link: TangleAddress
) -> Result<()> {

    //--------------------------------------------------------------
    // GENERATE GROUPS OF TRANSACATING NODES AND WITNESSES 1
    //--------------------------------------------------------------

    let (mut transacting_clients, mut witness_clients) = generate_trans_and_witnesses(&mut participants, average_proximity, witness_floor)?;

    //--------------------------------------------------------------
    // GENERATE CONTRACT
    //--------------------------------------------------------------

    println!("Generating contract:");
    let contract = generate_contract::generate_contract(&mut transacting_clients)?;
    println!("-- Contract generated\n");

    //--------------------------------------------------------------
    // PERFORM THE TRANSACTION WITH CONTRACT
    //--------------------------------------------------------------

    transact(
        contract,
        &mut transacting_clients,
        &mut witness_clients,
        &mut on_a,
        announcement_link,
        lazy_method
    ).await?;

    // put the particpants back into the original array
    participants.append(&mut transacting_clients);
    participants.append(&mut witness_clients);

    return Ok(());
}

// Generates the transacting nodes and the witnesses for the next simulation
pub fn generate_trans_and_witnesses(
    participants: &mut Vec<ParticipantIdentity>,
    average_proximity: f32,
    witness_floor: usize
) -> Result<(Vec<ParticipantIdentity>,Vec<ParticipantIdentity>)> {

    let mut transacting_clients_1: Vec<ParticipantIdentity> = Vec::new();
    let mut witness_clients_1: Vec<ParticipantIdentity> = Vec::new();

    // we select the initiating transacting participant as the first participant
    transacting_clients_1.push(participants.remove(0));
    
    // The initiating transacting participant searches for another to transact with.
    // Using mod, this section will only finish when one is found, representing the start
    // of the process
    let mut count = 0;
    loop {
        if average_proximity > rand::thread_rng().gen() {
            transacting_clients_1.push(participants.remove(count % participants.len()));
            break;
        }
        count = count + 1;
    }

    // The transacting participants now search for witnesses and combine their results.
    // Each iteration of the upper loop is one of the transacting nodes searching for
    // witnesses. We must work with indexes instead of actual objects to removing potential
    // witnesses from the list for transacting nodes of indices larger than 0
    let tn_witnesses_lists: &mut Vec<Vec<usize>> = &mut Vec::new();

    println!("Selecting participants to be transacting nodes and witnesses:");
    for i in 0..transacting_clients_1.len(){
        println!("-- TN {} is finding witnesses:", i);
        let mut tn_witnesses: Vec<usize> = Vec::new();
        for j in 0..participants.len(){
            let rand: f32 = rand::thread_rng().gen();
            println!("---- Trying participant {}. Rand={}", j, rand);
            if average_proximity > rand {
                tn_witnesses.push(j);
                println!("------ Participant {} added", j);
            }
        }
        println!("---- Found witnesses at indices: {:?}\n", tn_witnesses);
        tn_witnesses_lists.push(tn_witnesses);
    }

    // The transacting participants combine their witnesses, and check if there are enough.
    // Using BTreeSet because it is ordered
    let mut set_of_witnesses: BTreeSet<&mut usize> = BTreeSet::new();
    for witnesses in tn_witnesses_lists{
        for witness in witnesses{
            set_of_witnesses.insert(witness);
        }
    }

    if set_of_witnesses.len() < witness_floor {
        panic!("Not enough witnesses were generated.")
    }

    // convert indices into objects (as it is ordered, we can account for
    // the changing indices)
    for (i, witness) in set_of_witnesses.iter().enumerate() {
        witness_clients_1.push(participants.remove(**witness - i))
    }

    return Ok((transacting_clients_1, witness_clients_1));
}
