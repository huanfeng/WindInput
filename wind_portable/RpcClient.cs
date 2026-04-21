using System;
using System.IO;
using System.IO.Pipes;
using System.Text;
using System.Threading;

namespace WindPortable
{
    class RpcClient
    {
        readonly string _pipeName;
        readonly int _timeoutMs;
        long _nextId;

        public RpcClient(string pipeName = null, int timeoutMs = 500)
        {
            _pipeName = pipeName ?? BuildVariant.RpcPipeName;
            _timeoutMs = timeoutMs;
        }

        public bool IsAvailable()
        {
            try
            {
                Call("System.Ping", "{}");
                return true;
            }
            catch
            {
                return false;
            }
        }

        public void Shutdown()
        {
            Call("System.Shutdown", "{}");
        }

        string Call(string method, string paramsJson)
        {
            using (var pipe = new NamedPipeClientStream(".", _pipeName, PipeDirection.InOut))
            {
                pipe.Connect(_timeoutMs);

                long id = Interlocked.Increment(ref _nextId);
                string reqJson = string.Format(
                    @"{{""v"":1,""id"":{0},""method"":""{1}"",""params"":{2}}}",
                    id, method, paramsJson);

                WriteMessage(pipe, reqJson);
                string respJson = ReadMessage(pipe);

                if (respJson.Contains("\"error\":\"") && !respJson.Contains("\"error\":\"\""))
                {
                    int start = respJson.IndexOf("\"error\":\"") + 9;
                    int end = respJson.IndexOf("\"", start);
                    if (end > start)
                        throw new Exception(respJson.Substring(start, end - start));
                }
                return respJson;
            }
        }

        static void WriteMessage(Stream stream, string json)
        {
            byte[] payload = Encoding.UTF8.GetBytes(json);
            byte[] header = new byte[4];
            header[0] = (byte)(payload.Length >> 24);
            header[1] = (byte)(payload.Length >> 16);
            header[2] = (byte)(payload.Length >> 8);
            header[3] = (byte)(payload.Length);
            stream.Write(header, 0, 4);
            stream.Write(payload, 0, payload.Length);
            stream.Flush();
        }

        static string ReadMessage(Stream stream)
        {
            byte[] header = ReadExact(stream, 4);
            int length = (header[0] << 24) | (header[1] << 16) | (header[2] << 8) | header[3];
            if (length > 16 * 1024 * 1024)
                throw new InvalidOperationException($"Message too large: {length} bytes");
            byte[] payload = ReadExact(stream, length);
            return Encoding.UTF8.GetString(payload);
        }

        static byte[] ReadExact(Stream stream, int count)
        {
            byte[] buf = new byte[count];
            int offset = 0;
            while (offset < count)
            {
                int read = stream.Read(buf, offset, count - offset);
                if (read == 0) throw new EndOfStreamException();
                offset += read;
            }
            return buf;
        }
    }
}
