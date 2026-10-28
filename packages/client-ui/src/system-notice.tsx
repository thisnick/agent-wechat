import "./system-notice.css";

export function SystemNotice({ text, timestamp, dateTime, messageId }: {
  text: string;
  timestamp: string;
  dateTime: string;
  messageId: string;
}) {
  return (
    <article className="message system-notice" data-message-id={messageId}>
      <span>{text}</span>
      <time className="message-time" dateTime={dateTime}>{timestamp}</time>
    </article>
  );
}
