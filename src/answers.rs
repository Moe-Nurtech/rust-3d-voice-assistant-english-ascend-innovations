
//! Fixed answers for Ascend information buttons.
//! The robot speaks these answers exactly as written.
//! No Ollama is used for these.
//!
//! Keep answers conversational and suitable for voice playback.
//! worker.rs converts technical terms and numbers into speakable words.

pub struct Answer {
    /// Text on the button, also shown as the "question" in the panel
    pub label: &'static str,
    /// What the robot says (keep under about 1000 characters)
    pub text: &'static str,
}

pub static PROGRAMMES: Answer = Answer {
    label: "Our Programmes",
    text: "Ascend offers three main learning pathways designed to develop future-ready technology skills. \
The Future Skills Foundation lasts three months and includes one module and one completed project. \
The Future Careers Foundation lasts six months and includes two modules and two projects. \
The Future Careers Acceleration lasts one year and includes four modules, four projects, and a learning portfolio. \
Students can explore Robotics and Artificial Intelligence or Game Design and Development. \
Every pathway focuses on practical learning and building real projects. \
Book a free demo class and our team will recommend the right programme.",
};

pub static ROBOTICS_AI: Answer = Answer {
    label: "Robotics & AI",
    text: "At Ascend, students discover how robots and intelligent machines work through hands-on projects. \
They explore LEGO SPIKE Prime, micro:bit, Arduino, Raspberry Pi, sensors, and electronics. \
As they progress, they learn Python programming, Artificial Intelligence, Machine Learning, Computer Vision, and the Internet of Things. \
Students can build smart devices, explore autonomous robotics, and create their own intelligent systems. \
No previous experience is required for beginner programmes. \
Book a free demo class to discover the exciting world of Robotics and AI.",
};

pub static GAMES: Answer = Answer {
    label: "Game Development",
    text: "At Ascend, students turn their imagination into playable games and interactive worlds. \
Beginners can start with Scratch and PictoBlox, then explore Minecraft Education and Roblox. \
More advanced learners can discover three-dimensional game asset creation, Blender, Unity, Unreal Engine, and Unreal Editor for Fortnite. \
Students learn game design, storytelling, programming, three-dimensional modelling, and world building. \
They develop their own games and creative projects as they progress. \
Book a free demo class and start creating your own games with Ascend.",
};

pub static LEARNING: Answer = Answer {
    label: "Learning Options",
    text: "Ascend welcomes children from age six, teenagers up to age sixteen, and adult learners. \
Classes are generally sixty minutes long and held twice a week. \
You can study at our academy in Mirdif, Dubai, or through our online learning programmes. \
On-site students have access to robotics laboratories, equipment, and gaming computers. \
Online learners can access our cloud lab for supported activities. \
Students receive an Ascend certificate for each eligible completed module. \
We also offer technology camps, workshops, and selected scholarship opportunities. \
Start with a free demo class to find the best learning option.",
};

pub static ABOUT: Answer = Answer {
    label: "About Ascend",
    text: "Ascend EdTech is a technology education academy in Dubai and part of Ascend Innovations. \
Our mission is to help children, teenagers, and adults develop future-ready skills through practical technology education. \
We specialise in two main areas: Robotics and Artificial Intelligence, and Game Design and Development. \
Our students learn by building, coding, experimenting, and creating real projects rather than simply consuming technology. \
You can find us at Unit L2-312, Sport Society Mall, Mirdif, Dubai. \
Call us on +971 4 229 6887 or WhatsApp +971 50 754 2891. \
You can also email academy@ascendesport.com or visit ascendedu.online. \
Contact our team to book your free demo class.",
};

/// All button answers (used to prepare their voices at startup)
pub fn all() -> [&'static Answer; 5] {
    [&PROGRAMMES, &ROBOTICS_AI, &GAMES, &LEARNING, &ABOUT]
}
