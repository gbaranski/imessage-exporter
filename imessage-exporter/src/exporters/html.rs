use std::{
    collections::HashMap,
    fs::{copy, File},
    io::Write,
    path::{Path, PathBuf},
};

use crate::{
    app::{converter::heic_to_jpeg, progress::build_progress_bar_export, runtime::Config},
    exporters::exporter::{Exporter, Writer},
};

use imessage_database::{
    error::plist::PlistParseError,
    message_types::{
        app::AppMessage,
        url::URLMessage,
        variants::{BalloonProvider, CustomBalloon},
    },
    tables::{
        attachment::MediaType,
        messages::BubbleType,
        table::{FITNESS_RECEIVER, ME, ORPHANED, UNKNOWN},
    },
    util::{dates, dirs::home, plist::parse_plist},
    Attachment, Variant, {BubbleEffect, Expressive, Message, ScreenEffect, Table},
};
use uuid::Uuid;

use super::exporter::BalloonFormatter;

const HEADER: &str = "<html>\n<meta charset=\"UTF-8\">";
const FOOTER: &str = "</html>";
const STYLE: &str = include_str!("resources/style.css");

pub struct HTML<'a> {
    /// Data that is setup from the application's runtime
    pub config: &'a Config<'a>,
    /// Handles to files we want to write messages to
    /// Map of internal unique chatroom ID to a filename
    pub files: HashMap<i32, PathBuf>,
    /// Path to file for orphaned messages
    pub orphaned: PathBuf,
}

impl<'a> Exporter<'a> for HTML<'a> {
    fn new(config: &'a Config) -> Self {
        let mut orphaned = config.export_path();
        orphaned.push(ORPHANED);
        orphaned.set_extension("html");
        HTML {
            config,
            files: HashMap::new(),
            orphaned,
        }
    }

    fn iter_messages(&mut self) -> Result<(), String> {
        // Tell the user what we are doing
        eprintln!(
            "Exporting to {} as html...",
            self.config.export_path().display()
        );

        // Write orphaned file headers
        HTML::write_headers(&self.orphaned);

        // Set up progress bar
        let mut current_message = 0;
        let total_messages = Message::get_count(&self.config.db);
        let pb = build_progress_bar_export(total_messages);

        let mut statement = Message::get(&self.config.db);

        let messages = statement
            .query_map([], |row| Ok(Message::from_row(row)))
            .unwrap();

        for message in messages {
            let msg = Message::extract(message)?;
            if msg.is_annoucement() {
                let annoucement = self.format_annoucement(&msg);
                HTML::write_to_file(self.get_or_create_file(&msg), &annoucement);
            }
            // Message replies and reactions are rendered in context, so no need to render them separately
            else if !msg.is_reaction() {
                let message = self.format_message(&msg, 0)?;
                HTML::write_to_file(self.get_or_create_file(&msg), &message);
            }
            current_message += 1;
            pb.set_position(current_message);
        }
        pb.finish();

        eprintln!("Writing HTML footers...");
        self.files
            .iter()
            .for_each(|(_, path)| HTML::write_to_file(path, FOOTER));
        HTML::write_to_file(&self.orphaned, FOOTER);

        Ok(())
    }

    /// Create a file for the given chat, caching it so we don't need to build it later
    fn get_or_create_file(&mut self, message: &Message) -> &Path {
        match self.config.conversation(message.chat_id) {
            Some((chatroom, id)) => self.files.entry(*id).or_insert_with(|| {
                let mut path = self.config.export_path();
                path.push(self.config.filename(chatroom));
                path.set_extension("html");

                // If the file already exists , don't write the headers again
                // This can happen if multiple chats use the same group name
                if !path.exists() {
                    // Write headers if the file does not exist
                    HTML::write_headers(&path);
                }

                path
            }),
            None => &self.orphaned,
        }
    }
}

impl<'a> Writer<'a> for HTML<'a> {
    fn format_message(&self, message: &Message, indent: usize) -> Result<String, String> {
        // Data we want to write to a file
        let mut formatted_message = String::new();

        // Message div
        self.add_line(&mut formatted_message, "<div class=\"message\">", "", "");

        // Start message div
        if message.is_from_me {
            self.add_line(
                &mut formatted_message,
                &format!("<div class=\"sent {:?}\">", message.service()),
                "",
                "",
            );
        } else {
            self.add_line(&mut formatted_message, "<div class=\"received\">", "", "");
        }

        // Add message date
        self.add_line(
            &mut formatted_message,
            &self.get_time(message),
            "<p><span class=\"timestamp\">",
            "</span>",
        );

        // Add message sender
        self.add_line(
            &mut formatted_message,
            self.config.who(&message.handle_id, message.is_from_me),
            "<span class=\"sender\">",
            "</span></p>",
        );

        // Useful message metadata
        let message_parts = message.body();
        let mut attachments = Attachment::from_message(&self.config.db, message)?;
        let replies = message.get_replies(&self.config.db)?;
        let reactions = message.get_reactions(&self.config.db, &self.config.reactions)?;

        // Index of where we are in the attachment Vectorddddfgfgfdfdd
        let mut attachment_index: usize = 0;

        if let Some(subject) = &message.subject {
            // Add message sender
            self.add_line(
                &mut formatted_message,
                subject,
                "<p>Subject: <span class=\"subject\">",
                "</span></p>",
            );
        }

        // Generate the message body from it's components
        for (idx, message_part) in message_parts.iter().enumerate() {
            // Write the part div start
            self.add_line(
                &mut formatted_message,
                "<hr><div class=\"message_part\">",
                "",
                "",
            );

            match message_part {
                BubbleType::Text(text) => match text.starts_with(FITNESS_RECEIVER) {
                    true => self.add_line(
                        &mut formatted_message,
                        &text.replace(FITNESS_RECEIVER, "You"),
                        "<span class=\"bubble\">",
                        "</span>",
                    ),
                    false => self.add_line(
                        &mut formatted_message,
                        *text,
                        "<span class=\"bubble\">",
                        "</span>",
                    ),
                },
                BubbleType::Attachment => {
                    match attachments.get_mut(attachment_index) {
                        Some(attachment) => match self.format_attachment(attachment) {
                            Ok(result) => {
                                attachment_index += 1;
                                self.add_line(&mut formatted_message, &result, "", "");
                            }
                            Err(result) => {
                                self.add_line(
                                    &mut formatted_message,
                                    &result,
                                    "<span class=\"attachment_error\">Unable to locate attachment: ",
                                    "</span>",
                                );
                            }
                        },
                        // Attachment does not exist in attachments table
                        None => self.add_line(
                            &mut formatted_message,
                            "Attachment does not exist!",
                            "",
                            "",
                        ),
                    }
                }
                // TODO: Support app messages
                BubbleType::App => match self.format_app(message, &mut attachments) {
                    Ok(ok_bubble) => self.add_line(
                        &mut formatted_message,
                        &ok_bubble,
                        "<div class=\"app\">",
                        "</div>",
                    ),
                    Err(why) => self.add_line(
                        &mut formatted_message,
                        &format!("Unable to format app message: {why}"),
                        "<div class=\"app_error\">",
                        "</div>",
                    ),
                },
            };

            // Write the part div end
            self.add_line(&mut formatted_message, "</div>", "", "");

            // Handle expressives
            if message.expressive_send_style_id.is_some() {
                self.add_line(
                    &mut formatted_message,
                    self.format_expressive(message),
                    "<span class=\"expressive\">",
                    "</span>",
                );
            }

            // Handle Reactions
            if let Some(reactions) = reactions.get(&idx) {
                self.add_line(
                    &mut formatted_message,
                    "<hr><p>Reactions:</p>",
                    "<div class=\"reactions\">",
                    "",
                );
                reactions
                    .iter()
                    .try_for_each(|reaction| -> Result<(), String> {
                        self.add_line(
                            &mut formatted_message,
                            &self.format_reaction(reaction)?,
                            "<div class=\"reaction\">",
                            "</div>",
                        );
                        Ok(())
                    })?;
                self.add_line(&mut formatted_message, "</div>", "", "")
            }

            // Handle Replies
            if let Some(replies) = replies.get(&idx) {
                self.add_line(&mut formatted_message, "<div class=\"replies\">", "", "");
                replies.iter().try_for_each(|reply| -> Result<(), String> {
                    if !reply.is_reaction() {
                        // Set indent to 1 so we know this is a recursive call
                        self.add_line(
                            &mut formatted_message,
                            &self.format_message(reply, 1)?,
                            "<div class=\"reply\">",
                            "</div>",
                        );
                    }
                    Ok(())
                })?;
                self.add_line(&mut formatted_message, "</div>", "", "")
            }
        }

        // Add a note if the message is a reply and not rendered in a thread
        if message.is_reply() && indent == 0 {
            self.add_line(
                &mut formatted_message,
                "This message responded to an earlier message.",
                "<span class=\"reply_context\">",
                "</span>",
            );
        }

        // End message type div
        self.add_line(&mut formatted_message, "</div>", "", "");

        // End message div
        self.add_line(&mut formatted_message, "</div>", "", "");

        Ok(formatted_message)
    }

    fn format_attachment(&self, attachment: &'a mut Attachment) -> Result<String, &'a str> {
        match attachment.path() {
            Some(path) => {
                if let Some(path_str) = path.as_os_str().to_str() {
                    // Resolve the attachment path if necessary
                    // TODO: can we avoid copying the path here?
                    let resolved_attachment_path = match path.starts_with("~") {
                        true => path_str.replace("~", &home()),
                        false => path_str.to_owned(),
                    };

                    // Perform optional copy + convert
                    if !self.config.options.no_copy {
                        let qualified_attachment_path = Path::new(&resolved_attachment_path);
                        match attachment.extension() {
                            Some(ext) => {
                                // Create a path to copy the file to
                                let mut copy_path = self.config.attachment_path();
                                copy_path.push(Uuid::new_v4().to_string());
                                // If the image is a HEIC, convert it to PNG, otherwise perform the copy
                                if ext == "heic" || ext == "HEIC" {
                                    // Write the converted file
                                    copy_path.set_extension("jpg");
                                    match heic_to_jpeg(qualified_attachment_path, &copy_path) {
                                        Some(_) => {}
                                        None => {
                                            // It is kind of odd to use Ok() on the failure here, but the Err()
                                            // this function returns is used for when files are missing, not when
                                            // conversion fails. Perhaps this should be a Result<String, Enum>
                                            // of some kind, but this conversion failure is quite rare.
                                            return Ok(format!(
                                                "Unable to convert and display file: {}",
                                                &attachment.transfer_name
                                            ));
                                        }
                                    }
                                } else {
                                    // Just copy the file
                                    copy_path.set_extension(ext);
                                    if qualified_attachment_path.exists() {
                                        copy(qualified_attachment_path, &copy_path).unwrap();
                                    } else {
                                        return Err(&attachment.transfer_name);
                                    }
                                }
                                // Update the attachment
                                attachment.copied_path =
                                    Some(copy_path.to_string_lossy().to_string());
                            }
                            None => {
                                return Err(&attachment.transfer_name);
                            }
                        }
                    }

                    let embed_path = match &attachment.copied_path {
                        Some(path) => &path,
                        None => &resolved_attachment_path,
                    };

                    Ok(match attachment.mime_type() {
                        MediaType::Image(_) => {
                            format!("<img src=\"{embed_path}\" loading=\"lazy\">")
                        }
                        MediaType::Video(media_type) => {
                            format!("<video controls> <source src=\"{embed_path}\" type=\"{media_type}\"> </video>")
                        }
                        MediaType::Audio(media_type) => {
                            format!("<audio controls src=\"{embed_path}\" type=\"{media_type}\" </audio>")
                        }
                        MediaType::Text(_) => {
                            format!(
                                "<a href=\"file://{embed_path}\">Click to download {}</a>",
                                attachment.transfer_name
                            )
                        }
                        MediaType::Application(_) => format!(
                            "<a href=\"file://{embed_path}\">Click to download {}</a>",
                            attachment.transfer_name
                        ),
                        MediaType::Unknown => {
                            format!("<p>Unknown attachment type: {embed_path}</p>")
                        }
                        MediaType::Other(media_type) => {
                            format!("<p>Unable to embed {media_type} attachments: {embed_path}</p>")
                        }
                    })
                } else {
                    return Err(&attachment.transfer_name);
                }
            }
            None => Err(&attachment.transfer_name),
        }
    }

    fn format_app(
        &self,
        message: &'a Message,
        attachments: &mut Vec<Attachment>,
    ) -> Result<String, PlistParseError> {
        if let Variant::App(balloon) = message.variant() {
            let mut app_bubble = String::new();

            match message.payload_data(&self.config.db) {
                Some(payload) => {
                    let parsed = parse_plist(&payload)?;

                    let res = if message.is_url() {
                        match URLMessage::from_map(&parsed) {
                            Ok(bubble) => self.format_url(&bubble),
                            Err(why) => {
                                // If we didn't parse the URL blob, try and get the message text, which may contain the URL
                                if let Some(text) = &message.text {
                                    return Ok(text.to_string());
                                }
                                return Err(PlistParseError::ParseError(format!("{why}")));
                            }
                        }
                    } else {
                        match AppMessage::from_map(&parsed) {
                            Ok(bubble) => match balloon {
                                CustomBalloon::Application(bundle_id) => {
                                    self.format_generic_app(&bubble, bundle_id, attachments)
                                }
                                CustomBalloon::Handwriting => self.format_handwriting(&bubble),
                                CustomBalloon::ApplePay => self.format_apple_pay(&bubble),
                                CustomBalloon::Fitness => self.format_fitness(&bubble),
                                CustomBalloon::Slideshow => self.format_slideshow(&bubble),
                                CustomBalloon::URL => unreachable!(),
                            },
                            Err(why) => return Err(PlistParseError::ParseError(format!("{why}"))),
                        }
                    };
                    app_bubble.push_str(&res);
                }
                None => {
                    // Sometimes, URL messages are missing their payloads
                    if message.is_url() {
                        if let Some(text) = &message.text {
                            return Ok(text.to_string());
                        }
                    }
                    return Err(PlistParseError::NoPayload);
                }
            }
            Ok(app_bubble)
        } else {
            Err(PlistParseError::WrongMessageType)
        }
    }

    fn format_reaction(&self, msg: &Message) -> Result<String, String> {
        match msg.variant() {
            imessage_database::Variant::Reaction(_, added, reaction) => {
                if !added {
                    return Ok("".to_string());
                }
                Ok(format!(
                    "<span class=\"reaction\"><b>{:?}</b> by {}</span>",
                    reaction,
                    self.config.who(&msg.handle_id, msg.is_from_me),
                ))
            }
            imessage_database::Variant::Sticker(_) => {
                let mut paths = Attachment::from_message(&self.config.db, msg)?;
                // Sticker messages have only one attachment, the sticker image
                Ok(match paths.get_mut(0) {
                    Some(sticker) => match self.format_attachment(sticker) {
                        Ok(img) => {
                            let who = self.config.who(&msg.handle_id, msg.is_from_me);
                            Some(format!("{img}<span class=\"reaction\"> from {who}</span>"))
                        }
                        Err(_) => None,
                    },
                    None => None,
                }
                .unwrap_or(format!(
                    "<span class=\"reaction\">Sticker not found!</span>"
                )))
            }
            _ => unreachable!(),
        }
    }

    fn format_expressive(&self, msg: &'a Message) -> &'a str {
        match msg.get_expressive() {
            Expressive::Screen(effect) => match effect {
                ScreenEffect::Confetti => "Sent with Confetti",
                ScreenEffect::Echo => "Sent with Echo",
                ScreenEffect::Fireworks => "Sent with Fireworks",
                ScreenEffect::Balloons => "Sent with Balloons",
                ScreenEffect::Heart => "Sent with Heart",
                ScreenEffect::Lasers => "Sent with Lasers",
                ScreenEffect::ShootingStar => "Sent with Shooting Start",
                ScreenEffect::Sparkles => "Sent with Sparkles",
                ScreenEffect::Spotlight => "Sent with Spotlight",
            },
            Expressive::Bubble(effect) => match effect {
                BubbleEffect::Slam => "Sent with Slam",
                BubbleEffect::Loud => "Sent with Loud",
                BubbleEffect::Gentle => "Sent with Gentle",
                BubbleEffect::InvisibleInk => "Sent with Invisible Ink",
            },
            Expressive::Unknown(effect) => effect,
            Expressive::Normal => "",
        }
    }

    fn format_annoucement(&self, msg: &'a Message) -> String {
        let mut who = self.config.who(&msg.handle_id, msg.is_from_me);
        // Rename yourself so we render the proper grammar here
        if who == ME {
            who = "You"
        }
        let timestamp = dates::format(&msg.date(&self.config.offset));
        format!(
            "\n<div class =\"announcement\"><p><span class=\"timestamp\">{timestamp}</span> {who} named the conversation <b>{}</b></p></div>\n",
            msg.group_title.as_deref().unwrap_or(UNKNOWN)
        )
    }
    fn write_to_file(file: &Path, text: &str) {
        let mut file = File::options()
            .append(true)
            .create(true)
            .open(file)
            .unwrap();
        file.write_all(text.as_bytes()).unwrap();
    }
}

impl<'a> BalloonFormatter for HTML<'a> {
    fn format_url(&self, balloon: &URLMessage) -> String {
        let mut out_s = String::new();

        // Make the whole bubble clickable
        if let Some(url) = balloon.url {
            out_s.push_str("<a href=\"");
            out_s.push_str(url);
            out_s.push_str("\">");
        }

        // Header section
        out_s.push_str("<div class=\"app_header\">");

        // Add preview images
        balloon.images.iter().for_each(|image| {
            out_s.push_str("<img src=\"");
            out_s.push_str(image);
            out_s.push_str("\" loading=\"lazy\">");
        });

        // Header end
        out_s.push_str("</div>");

        // Only write the footer if there is data to write
        if balloon.title.is_some() || balloon.summary.is_some() {
            out_s.push_str("<div class=\"app_footer\">");

            // Title
            if let Some(title) = balloon.title {
                out_s.push_str("<div class=\"caption\">");
                out_s.push_str(title);
                out_s.push_str("</div>");
            }

            // Subtitle
            if let Some(summary) = balloon.summary {
                out_s.push_str("<div class=\"subcaption\"><xmp>");
                out_s.push_str(summary);
                out_s.push_str("</xmp></div>");
            }

            // End footer
            out_s.push_str("</div>");
        }

        // End the link
        if balloon.url.is_some() {
            out_s.push_str("</a>");
        }
        out_s
    }

    fn format_handwriting(&self, _: &AppMessage) -> String {
        String::from("Handwritten messages are not yet supported!")
    }

    fn format_apple_pay(&self, balloon: &AppMessage) -> String {
        self.balloon_to_html(balloon, "Apple Pay", &mut vec![])
    }

    fn format_fitness(&self, balloon: &AppMessage) -> String {
        self.balloon_to_html(balloon, "Fitness", &mut vec![])
    }

    fn format_slideshow(&self, balloon: &AppMessage) -> String {
        self.balloon_to_html(balloon, "Slideshow", &mut vec![])
    }

    fn format_generic_app(
        &self,
        balloon: &AppMessage,
        bundle_id: &str,
        attachments: &mut Vec<Attachment>,
    ) -> String {
        self.balloon_to_html(balloon, bundle_id, attachments)
    }
}

impl<'a> HTML<'a> {
    fn get_time(&self, message: &Message) -> String {
        let mut date = dates::format(&message.date(&self.config.offset));
        let read_after = message.time_until_read(&self.config.offset);
        if let Some(time) = read_after {
            if !time.is_empty() {
                let who = match message.is_from_me {
                    true => "them",
                    false => "you",
                };
                date.push_str(&format!(" (Read by {who} after {time})"));
            }
        }
        date
    }

    fn add_line(&self, string: &mut String, part: &str, pre: &str, post: &str) {
        if !part.is_empty() {
            string.push_str(pre);
            string.push_str(part);
            string.push_str(post);
            string.push('\n');
        }
    }

    fn write_headers(path: &Path) {
        // Write file header
        HTML::write_to_file(path, HEADER);

        // Write CSS
        HTML::write_to_file(path, "<style>\n");
        HTML::write_to_file(path, STYLE);
        HTML::write_to_file(path, "\n</style>");
    }

    fn balloon_to_html(
        &self,
        balloon: &AppMessage,
        bundle_id: &str,
        attachments: &mut Vec<Attachment>,
    ) -> String {
        let mut out_s = String::new();
        if let Some(url) = balloon.url {
            out_s.push_str("<a href=\"");
            out_s.push_str(url);
            out_s.push_str("\">");
        }
        out_s.push_str("<div class=\"app_header\">");

        // Image
        if let Some(image) = balloon.image {
            out_s.push_str("<img src=\"");
            out_s.push_str(image);
            out_s.push_str("\">");
        } else if let Some(attachment) = attachments.get_mut(0) {
            out_s.push_str(&self.format_attachment(attachment).unwrap_or_default());
        }

        // Name
        out_s.push_str("<div class=\"name\">");
        out_s.push_str(balloon.app_name.unwrap_or(bundle_id));
        out_s.push_str("</div>");

        // Title
        if let Some(title) = balloon.title {
            out_s.push_str("<div class=\"image_title\">");
            out_s.push_str(title);
            out_s.push_str("</div>");
        }

        // Subtitle
        if let Some(subtitle) = balloon.subtitle {
            out_s.push_str("<div class=\"image_subtitle\">");
            out_s.push_str(subtitle);
            out_s.push_str("</div>");
        }

        // ldtext
        if let Some(ldtext) = balloon.ldtext {
            out_s.push_str("<div class=\"ldtext\">");
            out_s.push_str(ldtext);
            out_s.push_str("</div>");
        }

        // Header end, footer begin
        out_s.push_str("</div>");

        // Only write the footer if there is data to write
        if balloon.caption.is_some()
            || balloon.subcaption.is_some()
            || balloon.trailing_caption.is_some()
            || balloon.trailing_subcaption.is_some()
        {
            out_s.push_str("<div class=\"app_footer\">");

            // Caption
            if let Some(caption) = balloon.caption {
                out_s.push_str("<div class=\"caption\">");
                out_s.push_str(caption);
                out_s.push_str("</div>");
            }

            // Subcaption
            if let Some(subcaption) = balloon.subcaption {
                out_s.push_str("<div class=\"subcaption\">");
                out_s.push_str(subcaption);
                out_s.push_str("</div>");
            }

            // Trailing Caption
            if let Some(trailing_caption) = balloon.trailing_caption {
                out_s.push_str("<div class=\"trailing_caption\">");
                out_s.push_str(trailing_caption);
                out_s.push_str("</div>");
            }

            // Trailing Subcaption
            if let Some(trailing_subcaption) = balloon.trailing_subcaption {
                out_s.push_str("<div class=\"trailing_subcaption\">");
                out_s.push_str(trailing_subcaption);
                out_s.push_str("</div>");
            }

            out_s.push_str("</div>");
        }
        if balloon.url.is_some() {
            out_s.push_str("</a>");
        }
        out_s
    }
}

#[cfg(test)]
mod tests {
    use crate::{Config, Exporter, Options, HTML};
    use imessage_database::{tables::messages::Message, util::dirs::default_db_path};

    fn blank() -> Message {
        Message {
            rowid: i32::default(),
            guid: String::default(),
            text: None,
            service: Some("iMessage".to_string()),
            handle_id: i32::default(),
            subject: None,
            date: i64::default(),
            date_read: i64::default(),
            date_delivered: i64::default(),
            is_from_me: false,
            is_read: false,
            group_title: None,
            associated_message_guid: None,
            associated_message_type: i32::default(),
            balloon_bundle_id: None,
            expressive_send_style_id: None,
            thread_originator_guid: None,
            thread_originator_part: None,
            chat_id: None,
            num_attachments: 0,
            num_replies: 0,
        }
    }

    fn fake_options() -> Options<'static> {
        Options {
            db_path: default_db_path(),
            no_copy: true,
            diagnostic: false,
            export_type: Some("html"),
            export_path: None,
            valid: true,
        }
    }

    #[test]
    fn can_create() {
        let options = fake_options();
        let config = Config::new(options).unwrap();
        let exporter = HTML::new(&config);
        assert_eq!(exporter.files.len(), 0);
    }

    #[test]
    fn can_get_time_valid() {
        // Create exporter
        let options = fake_options();
        let config = Config::new(options).unwrap();
        let exporter = HTML::new(&config);

        // Create fake message
        let mut message = blank();
        // May 17, 2022  8:29:42 PM
        message.date = 674526582885055488;
        // May 17, 2022  8:29:42 PM
        message.date_delivered = 674526582885055488;
        // May 17, 2022  9:30:31 PM
        message.date_read = 674530231992568192;

        assert_eq!(
            exporter.get_time(&message),
            "May 17, 2022  5:29:42 PM (Read by you after 1 hour, 49 seconds)"
        );
    }

    #[test]
    fn can_get_time_invalid() {
        // Create exporter
        let options = fake_options();
        let config = Config::new(options).unwrap();
        let exporter = HTML::new(&config);

        // Create fake message
        let mut message = blank();
        // May 17, 2022  9:30:31 PM
        message.date = 674530231992568192;
        // May 17, 2022  9:30:31 PM
        message.date_delivered = 674530231992568192;
        // Wed May 18 2022 02:36:24 GMT+0000
        message.date_read = 674526582885055488;
        assert_eq!(exporter.get_time(&message), "May 17, 2022  6:30:31 PM");
    }

    #[test]
    fn can_add_line_no_indent() {
        // Create exporter
        let options = fake_options();
        let config = Config::new(options).unwrap();
        let exporter = HTML::new(&config);

        // Create sample data
        let mut s = String::new();
        exporter.add_line(&mut s, "hello world", "", "");

        assert_eq!(s, "hello world\n".to_string());
    }

    #[test]
    fn can_add_line() {
        // Create exporter
        let options = fake_options();
        let config = Config::new(options).unwrap();
        let exporter = HTML::new(&config);

        // Create sample data
        let mut s = String::new();
        exporter.add_line(&mut s, "hello world", "  ", "");

        assert_eq!(s, "  hello world\n".to_string());
    }

    #[test]
    fn can_add_line_pre_post() {
        // Create exporter
        let options = fake_options();
        let config = Config::new(options).unwrap();
        let exporter = HTML::new(&config);

        // Create sample data
        let mut s = String::new();
        exporter.add_line(&mut s, "hello world", "<div>", "</div>");

        assert_eq!(s, "<div>hello world</div>\n".to_string());
    }
}
