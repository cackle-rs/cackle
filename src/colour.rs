use clap::ValueEnum;

#[derive(ValueEnum, Debug, Clone, Copy, Default)]
pub(crate) enum Colour {
    #[default]
    Auto,
    Always,
    Never,
}

impl Colour {
    pub(crate) fn should_use_colour(&self) -> bool {
        match self {
            Colour::Auto => panic!("Missing call to Colour::detect"),
            Colour::Always => true,
            Colour::Never => false,
        }
    }

    /// Resolves "auto" to either "always" or "never" depending on if the output is a tty. Also
    /// updates the colored crate's override if the flag was already set to "never" or "always".
    pub(crate) fn detect(self) -> Self {
        match self {
            Colour::Auto => {
                if atty::is(atty::Stream::Stdout) {
                    Colour::Always
                } else {
                    Colour::Never
                }
            }
            Colour::Always => {
                colored::control::set_override(true);
                self
            }
            Colour::Never => {
                colored::control::set_override(false);
                self
            }
        }
    }
}
