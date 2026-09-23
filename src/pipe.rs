//! The dedicated channel the host and Coderpack talk over.
//!
//! The pair used to talk on the JVM's stdin and stdout, which works until
//! something else in that process writes a line.  A mod calling `println`, a
//! logging framework pointed at the console, `-Xlog`, a crash dump: any of them
//! can land in the middle of a frame, and the frame that was cut in half is
//! gone.  A lost verdict stops the game thread until the host answers for it,
//! and a lost command costs a mod its full two-second timeout.
//!
//! So the frames get a channel of their own and stdout goes back to being
//! output.  Windows hands a named pipe out as an ordinary file, which is what
//! makes this cheap on both sides: the host gets something it can `read` and
//! `write`, and the JVM opens the same thing with `RandomAccessFile` and no
//! native code.  Anonymous pipes on Windows are named pipes underneath, so
//! nothing about the per-frame cost changes.
//!
//! Two pipes, `<base>.in` toward the JVM and `<base>.out` back, and not one
//! duplex pipe, which is what this was first written as.  A handle created
//! without `FILE_FLAG_OVERLAPPED` is a synchronous file object, and Windows
//! serialises operations on a file object: while the reader thread sits in
//! `ReadFile`, a write waits behind it forever.  Duplicating the handle does
//! not help, because a duplicate is another handle onto the same file object.
//! The symptom is worth knowing: exactly one frame gets through, and then the
//! two sides wait for each other.  One pipe per direction means no file object
//! is ever touched by more than one thread doing one thing.

#[cfg(windows)]
mod imp {
    use std::ffi::OsStr;
    use std::fs::File;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::FromRawHandle;
    use std::sync::mpsc::{RecvTimeoutError, channel};
    use std::thread;
    use std::time::{Duration, Instant};

    use anyhow::{Result, anyhow};
    use windows_sys::Win32::Foundation::{
        ERROR_PIPE_CONNECTED, GetLastError, HANDLE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Security::Cryptography::{
        BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX,
    };
    use windows_sys::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
        PIPE_TYPE_BYTE, PIPE_WAIT,
    };

    /// One frame is a short line.  64 KiB either way is far more than a burst
    /// of them needs and costs nothing while unused.
    const BUFFER: u32 = 64 * 1024;

    /// How long the JVM has to come to the pipes.  Generous: this covers a cold
    /// JVM start on a slow disk, and the wait ends the moment the child dies
    /// rather than on the clock, so nothing normal ever approaches it.
    const ARRIVAL: Duration = Duration::from_secs(60);

    pub struct Listener {
        toward: HANDLE,
        back: HANDLE,
        base: String,
    }

    // The handles are created on one thread, handed to another to wait on, and
    // then turned into Files.  Only one of them touches one at a time.
    unsafe impl Send for Listener {}
    unsafe impl Sync for Listener {}

    impl Listener {
        /// Creates both pipes and returns them unconnected.  Called before the
        /// JVM is started, because a client that arrives first finds nothing.
        pub fn bind() -> Result<Listener> {
            // Random, not derived from the pid: a name another process can
            // predict is a name it can reach first.  It cannot steal this one
            // either way, because FIRST_PIPE_INSTANCE refuses to create a pipe
            // under a name already taken, but then the host fails to start
            // instead of the JVM failing to connect, and only one of those two
            // messages is true.
            let base = format!("\\\\.\\pipe\\ancaria-{:032x}", random()?);
            Ok(Listener {
                toward: create(&format!("{base}.in"))?,
                back: create(&format!("{base}.out"))?,
                base,
            })
        }

        /// What to pass the JVM as `--pipe`.  It appends the two suffixes.
        pub fn name(&self) -> &str {
            &self.base
        }

        /// Waits for the JVM to open both pipes and hands back the host's ends,
        /// to read from and to write to.
        ///
        /// In the order the JVM opens them, so that neither side is waiting on
        /// the one the other has not reached yet.
        pub fn accept(self, running: &mut dyn FnMut() -> bool) -> Result<(File, File)> {
            let deadline = Instant::now() + ARRIVAL;
            let write = connect(self.toward, &self.base, running, deadline)?;
            let read = connect(self.back, &self.base, running, deadline)?;
            Ok((read, write))
        }
    }

    fn create(name: &str) -> Result<HANDLE> {
        let wide: Vec<u16> = OsStr::new(name).encode_wide().chain([0]).collect();
        // Duplex although each is used one way, because the JVM opens both
        // with RandomAccessFile, which asks for read and write whatever it
        // intends to do with them.
        let handle = unsafe {
            CreateNamedPipeW(
                wide.as_ptr(),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                // One instance, so the JVM this was made for is the only thing
                // that can ever be on the other end of it.
                1,
                BUFFER,
                BUFFER,
                0,
                std::ptr::null(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(anyhow!(
                "Could not create {name}: Windows error {}",
                unsafe { GetLastError() }
            ));
        }
        Ok(handle)
    }

    /// Waits for one pipe's client.
    ///
    /// `running` is polled rather than simply waiting out the clock: a JVM that
    /// failed to start is the likely reason nobody arrives, and "Coderpack
    /// exited before it connected" beats a minute of silence.
    fn connect(
        handle: HANDLE,
        base: &str,
        running: &mut dyn FnMut() -> bool,
        deadline: Instant,
    ) -> Result<File> {
        // A raw handle is a raw pointer and so is not Send on its own.
        // Carrying it across is safe here because the waiting thread is the
        // only thing that touches it until it answers.
        struct Waited(HANDLE);
        unsafe impl Send for Waited {}

        let carried = Waited(handle);
        let (done, waiting) = channel();
        // ConnectNamedPipe blocks, and there is no timeout on it worth having:
        // the overlapped form would leave the handle in a mode std::fs::File
        // cannot read.  So it waits on its own thread, and on the paths below
        // where nobody ever connects, that thread is still blocked when the
        // host exits and takes it with it.
        thread::spawn(move || {
            let carried = carried;
            let connected = unsafe { ConnectNamedPipe(carried.0, std::ptr::null_mut()) };
            // A client fast enough to be there already is a success that
            // reports itself as a failure.
            let ok = connected != 0 || unsafe { GetLastError() } == ERROR_PIPE_CONNECTED;
            let _ = done.send(ok);
        });

        loop {
            match waiting.recv_timeout(Duration::from_millis(50)) {
                Ok(true) => return Ok(unsafe { File::from_raw_handle(handle.cast()) }),
                Ok(false) | Err(RecvTimeoutError::Disconnected) => {
                    return Err(anyhow!("Coderpack could not be connected on {base}"));
                }
                Err(RecvTimeoutError::Timeout) => {
                    if !running() {
                        return Err(anyhow!(
                            "Coderpack exited before it connected to the host. \
                             Its own output above says why"
                        ));
                    }
                    if Instant::now() >= deadline {
                        return Err(anyhow!(
                            "Coderpack did not connect within {} seconds. \
                             A Coderpack older than this host does not know how, \
                             so check that launcher/zygote.jar was upgraded with it",
                            ARRIVAL.as_secs()
                        ));
                    }
                }
            }
        }
    }

    /// 128 bits from the system RNG, for the pipe name.
    fn random() -> Result<u128> {
        let mut bytes = [0u8; 16];
        let status = unsafe {
            BCryptGenRandom(
                std::ptr::null_mut(),
                bytes.as_mut_ptr(),
                bytes.len() as u32,
                BCRYPT_USE_SYSTEM_PREFERRED_RNG,
            )
        };
        if status != 0 {
            return Err(anyhow!("BCryptGenRandom failed with status {status:#x}"));
        }
        Ok(u128::from_le_bytes(bytes))
    }
}

#[cfg(not(windows))]
mod imp {
    use std::fs::File;

    use anyhow::{Result, anyhow};

    pub struct Listener;

    impl Listener {
        /// Named pipes are the Windows spelling of this, and the game is a
        /// Windows game.  Elsewhere the host falls back to stdio, which is what
        /// a failure here means.
        pub fn bind() -> Result<Listener> {
            Err(anyhow!("Named pipes are only implemented on Windows"))
        }

        pub fn name(&self) -> &str {
            ""
        }

        pub fn accept(self, _running: &mut dyn FnMut() -> bool) -> Result<(File, File)> {
            Err(anyhow!("Named pipes are only implemented on Windows"))
        }
    }
}

pub use imp::Listener;
